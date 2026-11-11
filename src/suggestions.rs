//! Data behind the Suggestions panel: every path of the input (loaded in the
//! background) and, when the query ends in a value position, the distinct
//! values found at that position.

use std::{
    collections::BTreeSet,
    ops::Bound,
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::{
    json_tree::{path_to_filter, Segment},
    value_completion::ValueCompletionRequest,
};

#[derive(Default)]
struct PathStore {
    /// Paths in sort order (see [`sort_key`]).
    paths: BTreeSet<String>,
    complete: bool,
}

/// Paths are stored with `]` replaced by a character sorting before digits, so
/// that `.users[1]` and its children come right after `.users[1` instead of
/// after `.users[10]`, `.users[100]`... (and are not cut off by the limit).
/// Paths never contain a raw `\x01`: control characters in keys are escaped.
fn sort_key(path: &str) -> String {
    path.replace(']', "\x01")
}

fn from_sort_key(key: &str) -> String {
    key.replace('\x01', "]")
}

/// Sorted set of all the paths of the input, shared with the loader thread.
#[derive(Clone, Default)]
pub struct PathIndex(Arc<Mutex<PathStore>>);

impl PathIndex {
    /// Extract all paths of `input` on a background thread, publishing them in chunks.
    /// `on_progress` is called after each chunk.
    pub fn spawn_loader(
        &self,
        input: Arc<Vec<Value>>,
        chunk_size: usize,
        on_progress: impl Fn() + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let index = self.clone();
        // A plain thread: it must not keep the runtime from shutting down on exit.
        std::thread::spawn(move || {
            let mut batch = Vec::with_capacity(chunk_size);
            let flush = |batch: &mut Vec<String>, complete: bool| {
                let mut store = index.0.lock().unwrap_or_else(|e| e.into_inner());
                store.paths.extend(batch.drain(..));
                store.complete = complete;
            };
            for_each_path(&input, &mut |path| {
                batch.push(sort_key(&path));
                if batch.len() >= chunk_size {
                    flush(&mut batch, false);
                    on_progress();
                }
            });
            flush(&mut batch, true);
            on_progress();
        })
    }

    /// Up to `limit` paths starting with `prefix`, and whether there are more.
    pub fn matches(&self, prefix: &str, limit: usize) -> (Vec<String>, bool) {
        let prefix = sort_key(prefix);
        let store = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Paths are sorted, so all matches form a contiguous range starting at `prefix`.
        let mut iter = store
            .paths
            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
            .take_while(|p| p.starts_with(prefix.as_str()));
        let items = iter
            .by_ref()
            .take(limit)
            .map(|p| from_sort_key(p))
            .collect();
        (items, iter.next().is_some())
    }

    pub fn is_complete(&self) -> bool {
        self.0.lock().map(|s| s.complete).unwrap_or(false)
    }
}

/// Call `f` with the jq path of every node of `input` (`.`, `.users`, `.users[0]`, ...).
fn for_each_path(input: &[Value], f: &mut impl FnMut(String)) {
    fn walk(value: &Value, segments: &mut Vec<Segment>, f: &mut impl FnMut(String)) {
        f(path_to_filter(segments));
        match value {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    segments.push(Segment::Index(i));
                    walk(item, segments, f);
                    segments.pop();
                }
            }
            Value::Object(map) => {
                for (name, item) in map {
                    segments.push(Segment::Name(name.clone()));
                    walk(item, segments, f);
                    segments.pop();
                }
            }
            _ => {}
        }
    }
    let mut segments = Vec::new();
    for value in input {
        walk(value, &mut segments, f);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Paths,
    Values,
    /// jq builtins, offered when no path matches.
    Functions,
}

/// Content of the Suggestions panel for the current query.
pub struct Suggestions {
    pub kind: SuggestionKind,
    pub items: Vec<String>,
    /// Query text each item is appended to (empty for paths, which are whole queries).
    pub head: String,
    /// Whether the list was cut at the configured maximum.
    pub truncated: bool,
    /// Explanation shown when the list is empty (still loading, error...).
    pub note: Option<String>,
    pub selected: usize,
    /// Query text before the user started previewing suggestions (restored on Esc).
    pub preview_origin: Option<String>,
    /// Values of the last value request, keyed by the jq filter producing them,
    /// so that refining the partial value (`"A` → `"Al`) doesn't re-run the filter.
    value_cache: Option<(String, Arc<Vec<String>>, bool)>,
}

impl Default for Suggestions {
    fn default() -> Self {
        Self {
            kind: SuggestionKind::Paths,
            items: Vec::new(),
            head: String::new(),
            truncated: false,
            note: None,
            selected: 0,
            preview_origin: None,
            value_cache: None,
        }
    }
}

impl Suggestions {
    pub fn set_paths(&mut self, items: Vec<String>, truncated: bool, loading: bool) {
        self.kind = SuggestionKind::Paths;
        self.head.clear();
        self.truncated = truncated;
        self.note = match (items.is_empty(), loading) {
            (true, true) => Some("Loading paths…".into()),
            (true, false) => Some("No matching path".into()),
            _ => None,
        };
        self.items = items;
        self.selected = 0;
    }

    /// Show jq builtins, each completing the query `head` + name.
    pub fn set_functions(&mut self, head: String, items: Vec<String>, truncated: bool) {
        self.kind = SuggestionKind::Functions;
        self.head = head;
        self.truncated = truncated;
        self.note = None;
        self.items = items;
        self.selected = 0;
    }

    /// The builtin whose documentation is shown, when listing functions.
    pub fn selected_function(&self) -> Option<&str> {
        match self.kind {
            SuggestionKind::Functions => self.items.get(self.selected).map(String::as_str),
            _ => None,
        }
    }

    /// Show values for `request` if they are cached. Returns false when they must be computed.
    pub fn set_values_from_cache(&mut self, request: &ValueCompletionRequest) -> bool {
        let Some((query, values, truncated)) = &self.value_cache else {
            return false;
        };
        if *query != request.source_query {
            return false;
        }
        let (values, truncated) = (values.clone(), *truncated);
        self.show_values(request, &values, truncated);
        true
    }

    pub fn set_values(
        &mut self,
        request: &ValueCompletionRequest,
        values: Vec<String>,
        truncated: bool,
    ) {
        let values = Arc::new(values);
        self.value_cache = Some((request.source_query.clone(), values.clone(), truncated));
        self.show_values(request, &values, truncated);
    }

    fn show_values(
        &mut self,
        request: &ValueCompletionRequest,
        values: &[String],
        truncated: bool,
    ) {
        self.kind = SuggestionKind::Values;
        self.head = request.head.clone();
        self.items = values
            .iter()
            .filter(|v| request.accepts(v))
            .cloned()
            .collect();
        self.truncated = truncated;
        self.note = self.items.is_empty().then(|| "No matching value".into());
        self.selected = 0;
    }

    pub fn set_values_pending(&mut self, request: &ValueCompletionRequest) {
        self.kind = SuggestionKind::Values;
        self.head = request.head.clone();
        self.items.clear();
        self.truncated = false;
        self.note = Some("Collecting values…".into());
        self.selected = 0;
    }

    pub fn set_values_error(&mut self, error: String) {
        self.kind = SuggestionKind::Values;
        self.items.clear();
        self.note = Some(format!("Values unavailable: {error}"));
        self.selected = 0;
    }

    /// The query obtained by accepting the selected item.
    pub fn selected_query(&self) -> Option<String> {
        self.items
            .get(self.selected)
            .map(|item| format!("{}{}", self.head, item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_indexed() {
        let input = crate::json::deserialize(r#"{"a":{"b c":[1]},"if":2}"#, None).unwrap();
        let index = PathIndex::default();
        index
            .spawn_loader(Arc::new(input), 2, || {})
            .join()
            .unwrap();
        assert!(index.is_complete());
        assert_eq!(
            index.matches(".", 10).0,
            vec![".", ".\"if\"", ".a", ".a.\"b c\"", ".a.\"b c\"[0]"]
        );
        assert_eq!(index.matches(".a", 1), (vec![".a".to_string()], true));
    }

    #[test]
    fn element_paths_come_before_longer_indices() {
        let items: Vec<i32> = (0..12).collect();
        let input = vec![serde_json::json!({ "xs": items })];
        let index = PathIndex::default();
        index
            .spawn_loader(Arc::new(input), 100, || {})
            .join()
            .unwrap();
        assert_eq!(
            index.matches(".xs[1", 3).0,
            vec![".xs[1]", ".xs[10]", ".xs[11]"]
        );
    }
}
