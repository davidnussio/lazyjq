//! Foldable, navigable view of a stream of JSON values (the Result panel).
//!
//! Every value is flattened into rows (one per scalar, opening and closing
//! bracket). Rows are always visited in document order, folding only hides
//! the rows between an opening bracket and its matching closing one, so
//! moving to the next/previous visible row is O(1).

use std::sync::Arc;

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use serde_json::Value;

use crate::config::JsonConfig;

const NO_PARENT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bracket {
    Object,
    Array,
}

impl Bracket {
    fn open(self) -> &'static str {
        match self {
            Bracket::Object => "{",
            Bracket::Array => "[",
        }
    }

    fn close(self) -> &'static str {
        match self {
            Bracket::Object => "}",
            Bracket::Array => "]",
        }
    }
}

#[derive(Debug)]
enum Key {
    /// Top-level value: index in the stream.
    Root(u32),
    Index(u32),
    Name(Box<str>),
    /// Closing brackets: their path is the one of the opening row.
    None,
}

#[derive(Clone, Copy, Debug)]
enum ScalarKind {
    Null,
    Bool,
    Number,
    String,
}

#[derive(Debug)]
enum Kind {
    Open {
        bracket: Bracket,
        close: u32,
        collapsed: bool,
        len: u32,
    },
    Close {
        bracket: Bracket,
        open: u32,
    },
    Empty(Bracket),
    Scalar(ScalarKind, Box<str>),
}

#[derive(Debug)]
struct Row {
    depth: u32,
    parent: u32,
    key: Key,
    kind: Kind,
    /// Whether the value ending on this row is followed by a comma.
    comma: bool,
}

/// One step of the path from a top-level value to a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Index(usize),
    Name(String),
}

pub struct JsonTree {
    rows: Vec<Row>,
    values: Arc<Vec<Value>>,
    cursor: usize,
    /// First visible row of the viewport.
    offset: usize,
}

impl JsonTree {
    pub fn new(values: Arc<Vec<Value>>) -> Self {
        let mut rows = Vec::new();
        for (i, value) in values.iter().enumerate() {
            push_rows(&mut rows, value, 0, NO_PARENT, Key::Root(i as u32), false);
        }
        Self {
            rows,
            values,
            cursor: 0,
            offset: 0,
        }
    }

    pub fn values(&self) -> &Arc<Vec<Value>> {
        &self.values
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn is_collapsed(&self, i: usize) -> bool {
        matches!(
            self.rows[i].kind,
            Kind::Open {
                collapsed: true,
                ..
            }
        )
    }

    fn next_visible(&self, i: usize) -> Option<usize> {
        let next = match self.rows[i].kind {
            Kind::Open {
                collapsed: true,
                close,
                ..
            } => close as usize + 1,
            _ => i + 1,
        };
        (next < self.rows.len()).then_some(next)
    }

    fn previous_visible(&self, i: usize) -> Option<usize> {
        let previous = i.checked_sub(1)?;
        match self.rows[previous].kind {
            Kind::Close { open, .. } if self.is_collapsed(open as usize) => Some(open as usize),
            _ => Some(previous),
        }
    }

    /// The visible row standing for `i`: `i` itself, or its outermost folded ancestor.
    fn anchor(&self, i: usize) -> usize {
        let mut anchor = match self.rows[i].kind {
            Kind::Close { open, .. } if self.is_collapsed(open as usize) => open as usize,
            _ => i,
        };
        let mut parent = self.rows[anchor].parent;
        while parent != NO_PARENT {
            if self.is_collapsed(parent as usize) {
                anchor = parent as usize;
            }
            parent = self.rows[parent as usize].parent;
        }
        anchor
    }

    pub fn up(&mut self, n: usize) {
        for _ in 0..n {
            match self.previous_visible(self.cursor) {
                Some(i) => self.cursor = i,
                None => break,
            }
        }
    }

    pub fn down(&mut self, n: usize) {
        if self.rows.is_empty() {
            return;
        }
        for _ in 0..n {
            match self.next_visible(self.cursor) {
                Some(i) => self.cursor = i,
                None => break,
            }
        }
    }

    pub fn top(&mut self) {
        self.cursor = 0;
    }

    pub fn bottom(&mut self) {
        if let Some(last) = self.rows.len().checked_sub(1) {
            self.cursor = self.anchor(last);
        }
    }

    /// Fold or unfold the container under the cursor.
    pub fn toggle(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        let open = match row.kind {
            Kind::Open { .. } => self.cursor,
            Kind::Close { open, .. } => open as usize,
            _ => return,
        };
        if let Kind::Open { collapsed, .. } = &mut self.rows[open].kind {
            *collapsed = !*collapsed;
        }
        self.cursor = open;
    }

    /// Unfold the node under the cursor, or step into it when already unfolded.
    pub fn expand(&mut self) {
        match self.rows.get_mut(self.cursor).map(|row| &mut row.kind) {
            Some(Kind::Open { collapsed, .. }) if *collapsed => *collapsed = false,
            Some(Kind::Open { .. }) => self.down(1),
            _ => {}
        }
    }

    /// Fold the node under the cursor, or move to its parent.
    pub fn collapse(&mut self) {
        match self.rows.get_mut(self.cursor).map(|row| &mut row.kind) {
            Some(Kind::Open { collapsed, .. }) if !*collapsed => *collapsed = true,
            Some(Kind::Close { open, .. }) => {
                let open = *open as usize;
                self.cursor = open;
                self.collapse();
            }
            Some(_) => {
                let parent = self.rows[self.cursor].parent;
                if parent != NO_PARENT {
                    self.cursor = parent as usize;
                }
            }
            None => {}
        }
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        for row in &mut self.rows {
            if let Kind::Open { collapsed: c, .. } = &mut row.kind {
                *c = collapsed;
            }
        }
        if !self.rows.is_empty() {
            self.cursor = self.anchor(self.cursor);
            self.offset = self.anchor(self.offset);
        }
    }

    /// Adjust the viewport so that the cursor is visible in `height` lines.
    pub fn scroll_into_view(&mut self, height: usize) {
        if self.rows.is_empty() || height == 0 {
            return;
        }
        self.offset = self.anchor(self.offset);
        if self.cursor <= self.offset {
            self.offset = self.cursor;
            return;
        }
        let mut row = self.offset;
        for _ in 1..height {
            match self.next_visible(row) {
                Some(next) if next <= self.cursor => row = next,
                _ => break,
            }
            if row == self.cursor {
                return;
            }
        }
        // The cursor is below the viewport: make it the last visible line.
        let mut offset = self.cursor;
        for _ in 1..height {
            match self.previous_visible(offset) {
                Some(previous) => offset = previous,
                None => break,
            }
        }
        self.offset = offset;
    }

    /// Visible rows of the viewport, in order.
    fn visible_rows(&self, height: usize) -> Vec<usize> {
        let mut rows = Vec::with_capacity(height);
        let mut row = (!self.rows.is_empty()).then_some(self.offset);
        while let Some(i) = row {
            if rows.len() >= height {
                break;
            }
            rows.push(i);
            row = self.next_visible(i);
        }
        rows
    }

    /// Move the cursor to the `line`-th line of the viewport (mouse click).
    pub fn select_line(&mut self, line: usize) {
        if let Some(&row) = self.visible_rows(line + 1).get(line) {
            self.cursor = row;
        }
    }

    /// Render the viewport. Returns the lines and the position of the cursor line among them.
    pub fn render(
        &self,
        height: usize,
        config: &JsonConfig,
        dim: Style,
    ) -> (Vec<Line<'static>>, Option<usize>) {
        let rows = self.visible_rows(height);
        let selected = rows.iter().position(|&i| i == self.cursor);
        let lines = rows
            .into_iter()
            .map(|i| self.render_row(i, config, dim))
            .collect();
        (lines, selected)
    }

    fn render_row(&self, i: usize, config: &JsonConfig, dim: Style) -> Line<'static> {
        let row = &self.rows[i];
        let mut spans = Vec::with_capacity(5);
        let indent = row.depth as usize * config.indent;
        if indent > 0 {
            spans.push(Span::raw(" ".repeat(indent)));
        }
        if let Key::Name(name) = &row.key {
            spans.push(Span::styled(json_string(name), config.key_style));
            spans.push(Span::raw(": "));
        }
        let mut comma = row.comma;
        match &row.kind {
            Kind::Open {
                bracket,
                collapsed: false,
                ..
            } => spans.push(Span::styled(bracket.open(), config.bracket_style)),
            Kind::Open {
                bracket,
                collapsed: true,
                close,
                len,
            } => {
                comma = self.rows[*close as usize].comma;
                spans.push(Span::styled(
                    format!("{}…{}", bracket.open(), bracket.close()),
                    config.bracket_style,
                ));
                let unit = match (bracket, *len == 1) {
                    (Bracket::Object, true) => "key",
                    (Bracket::Object, false) => "keys",
                    (Bracket::Array, true) => "item",
                    (Bracket::Array, false) => "items",
                };
                if comma {
                    spans.push(Span::raw(","));
                    comma = false;
                }
                spans.push(Span::styled(format!(" {len} {unit}"), dim));
            }
            Kind::Close { bracket, .. } => {
                spans.push(Span::styled(bracket.close(), config.bracket_style))
            }
            Kind::Empty(bracket) => spans.push(Span::styled(
                format!("{}{}", bracket.open(), bracket.close()),
                config.bracket_style,
            )),
            Kind::Scalar(kind, text) => {
                let style = match kind {
                    ScalarKind::Null => config.null_value_style,
                    ScalarKind::Bool => config.boolean_value_style,
                    ScalarKind::Number => config.number_value_style,
                    ScalarKind::String => config.string_value_style,
                };
                spans.push(Span::styled(text.to_string(), style));
            }
        }
        if comma {
            spans.push(Span::raw(","));
        }
        Line::from(spans)
    }

    /// Index of the top-level value and path from it to the node under the cursor.
    pub fn cursor_path(&self) -> Option<(usize, Vec<Segment>)> {
        let mut i = match self.rows.get(self.cursor)?.kind {
            Kind::Close { open, .. } => open as usize,
            _ => self.cursor,
        };
        let mut segments = Vec::new();
        loop {
            let row = &self.rows[i];
            match &row.key {
                Key::Root(root) => {
                    segments.reverse();
                    return Some((*root as usize, segments));
                }
                Key::Index(index) => segments.push(Segment::Index(*index as usize)),
                Key::Name(name) => segments.push(Segment::Name(name.to_string())),
                Key::None => return None,
            }
            i = row.parent as usize;
        }
    }

    /// The JSON value under the cursor.
    pub fn cursor_value(&self) -> Option<&Value> {
        let (root, segments) = self.cursor_path()?;
        let mut value = self.values.get(root)?;
        for segment in &segments {
            value = match segment {
                Segment::Index(i) => value.get(*i)?,
                Segment::Name(name) => value.get(name.as_str())?,
            };
        }
        Some(value)
    }

    /// The whole stream, pretty printed (one value after the other).
    pub fn formatted(&self, indent: usize) -> String {
        formatted_values(&self.values, indent)
    }
}

fn push_rows(rows: &mut Vec<Row>, value: &Value, depth: u32, parent: u32, key: Key, comma: bool) {
    let (bracket, len) = match value {
        Value::Array(a) if !a.is_empty() => (Bracket::Array, a.len()),
        Value::Object(o) if !o.is_empty() => (Bracket::Object, o.len()),
        Value::Array(_) | Value::Object(_) => {
            let bracket = if value.is_array() {
                Bracket::Array
            } else {
                Bracket::Object
            };
            rows.push(Row {
                depth,
                parent,
                key,
                kind: Kind::Empty(bracket),
                comma,
            });
            return;
        }
        scalar => {
            let (kind, text) = match scalar {
                Value::Null => (ScalarKind::Null, "null".to_string()),
                Value::Bool(b) => (ScalarKind::Bool, b.to_string()),
                Value::Number(n) => (ScalarKind::Number, n.to_string()),
                Value::String(s) => (ScalarKind::String, json_string(s)),
                _ => unreachable!(),
            };
            rows.push(Row {
                depth,
                parent,
                key,
                kind: Kind::Scalar(kind, text.into_boxed_str()),
                comma,
            });
            return;
        }
    };

    let open = rows.len() as u32;
    rows.push(Row {
        depth,
        parent,
        key,
        kind: Kind::Open {
            bracket,
            close: 0,
            collapsed: false,
            len: len as u32,
        },
        comma: false,
    });
    match value {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                push_rows(
                    rows,
                    item,
                    depth + 1,
                    open,
                    Key::Index(i as u32),
                    i + 1 < len,
                );
            }
        }
        Value::Object(map) => {
            for (i, (name, item)) in map.iter().enumerate() {
                let key = Key::Name(name.as_str().into());
                push_rows(rows, item, depth + 1, open, key, i + 1 < len);
            }
        }
        _ => unreachable!(),
    }
    let close = rows.len() as u32;
    rows.push(Row {
        depth,
        parent,
        key: Key::None,
        kind: Kind::Close { bracket, open },
        comma,
    });
    if let Kind::Open { close: c, .. } = &mut rows[open as usize].kind {
        *c = close;
    }
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

/// A stream of values pretty printed one after the other.
pub fn formatted_values(values: &[Value], indent: usize) -> String {
    values
        .iter()
        .map(|value| pretty(value, indent))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pretty print `value` with the given indentation width.
pub fn pretty(value: &Value, indent: usize) -> String {
    use serde::Serialize;
    let indent = " ".repeat(indent);
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    match value.serialize(&mut serializer) {
        Ok(()) => String::from_utf8(out).unwrap_or_default(),
        Err(_) => value.to_string(),
    }
}

/// jq keywords can't be used with the `.name` shorthand.
const JQ_KEYWORDS: [&str; 19] = [
    "and", "or", "not", "if", "then", "elif", "else", "end", "as", "def", "reduce", "foreach",
    "try", "catch", "label", "import", "include", "__loc__", "__prog__",
];

/// Format a path as a jq filter (`.users[0].name`, `."first name"`).
pub fn path_to_filter(segments: &[Segment]) -> String {
    if segments.is_empty() {
        return ".".to_string();
    }
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Index(i) => {
                if out.is_empty() {
                    out.push('.');
                }
                out.push_str(&format!("[{i}]"));
            }
            Segment::Name(name) => {
                out.push('.');
                if is_identifier(name) {
                    out.push_str(name);
                } else {
                    out.push_str(&json_string(name));
                }
            }
        }
    }
    out
}

pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !JQ_KEYWORDS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn tree(json: &str) -> JsonTree {
        JsonTree::new(Arc::new(crate::json::deserialize(json, None).unwrap()))
    }

    fn text(tree: &JsonTree, height: usize) -> Vec<String> {
        let config = Config::load_from("").unwrap();
        tree.render(height, &config.json, Style::default())
            .0
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    const SAMPLE: &str = r#"{"users":[{"name":"Bob","tags":[]},{"name":"Al"}],"n":null}"#;

    #[test]
    fn renders_like_pretty_json() {
        let t = tree(SAMPLE);
        assert_eq!(
            text(&t, 100),
            vec![
                "{",
                "  \"users\": [",
                "    {",
                "      \"name\": \"Bob\",",
                "      \"tags\": []",
                "    },",
                "    {",
                "      \"name\": \"Al\"",
                "    }",
                "  ],",
                "  \"n\": null",
                "}",
            ]
        );
    }

    #[test]
    fn folding_hides_children_and_navigation_skips_them() {
        let mut t = tree(SAMPLE);
        t.down(1); // "users": [
        t.toggle();
        assert_eq!(text(&t, 100)[1], "  \"users\": […], 2 items");
        t.down(1);
        assert_eq!(t.cursor_path().unwrap().1, vec![Segment::Name("n".into())]);
        t.up(1);
        assert_eq!(
            t.cursor_path().unwrap().1,
            vec![Segment::Name("users".into())]
        );
        t.bottom();
        assert_eq!(t.cursor(), t.row_count() - 1);
        t.set_all_collapsed(true);
        assert_eq!(t.cursor(), 0);
        assert_eq!(text(&t, 100), vec!["{…} 2 keys"]);
    }

    #[test]
    fn collapse_moves_to_parent() {
        let mut t = tree(SAMPLE);
        t.down(3); // "name": "Bob"
        t.collapse();
        assert_eq!(t.cursor(), 2);
        t.collapse();
        assert_eq!(text(&t, 3)[2], "    {…}, 2 keys");
        t.expand();
        t.expand();
        assert_eq!(t.cursor(), 3);
    }

    #[test]
    fn paths_and_values() {
        let mut t = tree(SAMPLE);
        t.down(7); // "name": "Al"
        let (root, path) = t.cursor_path().unwrap();
        assert_eq!(root, 0);
        assert_eq!(path_to_filter(&path), ".users[1].name");
        assert_eq!(t.cursor_value(), Some(&Value::from("Al")));
        t.down(1); // closing } of users[1]
        assert_eq!(path_to_filter(&t.cursor_path().unwrap().1), ".users[1]");
    }

    #[test]
    fn filter_formatting() {
        assert_eq!(path_to_filter(&[]), ".");
        assert_eq!(path_to_filter(&[Segment::Index(2)]), ".[2]");
        assert_eq!(
            path_to_filter(&[
                Segment::Name("first name".into()),
                Segment::Name("if".into())
            ]),
            ".\"first name\".\"if\""
        );
    }

    #[test]
    fn viewport_follows_cursor() {
        let mut t = tree(SAMPLE);
        t.down(5);
        t.scroll_into_view(3);
        assert_eq!(text(&t, 3)[2], "    },");
        t.up(5);
        t.scroll_into_view(3);
        assert_eq!(text(&t, 3)[0], "{");
    }

    #[test]
    fn multiple_roots() {
        let mut t = tree("1 [2]");
        t.down(2);
        assert_eq!(t.cursor_path(), Some((1, vec![Segment::Index(0)])));
        assert_eq!(t.formatted(2), "1\n[\n  2\n]");
    }
}
