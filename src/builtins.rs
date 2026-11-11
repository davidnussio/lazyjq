//! jq builtin functions offered by the Suggestions panel when no path matches,
//! with their documentation (taken from the jq manual) shown in place of History.

use std::sync::LazyLock;

/// Function names and their Markdown documentation, sorted by name.
///
/// Only the functions supported by jaq are listed (see the `all_builtins_compile` test).
static BUILTINS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(include_str!("builtins.json")).expect("valid builtins.json");
    map.into_iter()
        .map(|(name, doc)| (name, doc.as_str().unwrap_or_default().to_string()))
        .collect()
});

/// Documentation of the builtin `name`.
pub fn doc(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, doc)| doc.as_str())
}

/// Builtins that can complete `text`: the text they are appended to, and up to
/// `limit` names starting with the identifier being typed at the end of `text`
/// (all of them after a pipe, an opening parenthesis, an operator...).
/// Returns `None` when the end of the query is not a place for a function call
/// (a field name, a variable, a string...).
pub fn complete(text: &str, limit: usize) -> Option<(String, Vec<String>, bool)> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let (head, word) = text.split_at(text.trim_end_matches(is_ident).len());
    if word.starts_with(|c: char| c.is_ascii_digit()) || in_string(head) {
        return None;
    }
    if word.is_empty() {
        if !expects_filter(head) {
            return None;
        }
    } else if head.ends_with(['.', '$', '@']) {
        return None;
    }
    let mut names = BUILTINS
        .iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with(word));
    let items: Vec<String> = names.by_ref().take(limit).cloned().collect();
    let truncated = names.next().is_some();
    Some((head.to_string(), items, truncated))
}

/// The innermost builtin whose arguments are being typed at the end of `text`
/// (`select` for `.[] | select(.a == 1 and (.b`).
pub fn enclosing_call(text: &str) -> Option<&'static str> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // Open brackets, with the builtin called by each `(`.
    let mut open: Vec<Option<&'static str>> = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for (i, c) in text.char_indices() {
        if in_string {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => {
                let before = text[..i].trim_end();
                let name = &before[before.trim_end_matches(is_ident).len()..];
                let is_call = !before[..before.len() - name.len()].ends_with(['.', '$', '@']);
                open.push(
                    is_call
                        .then(|| BUILTINS.iter().find(|(n, _)| n == name))
                        .flatten()
                        .map(|(n, _)| n.as_str()),
                );
            }
            '[' | '{' => open.push(None),
            ')' | ']' | '}' => {
                open.pop();
            }
            _ => {}
        }
    }
    open.into_iter().rev().flatten().next()
}

/// Whether `text` ends inside a string literal.
fn in_string(text: &str) -> bool {
    let mut inside = false;
    let mut escaped = false;
    for c in text.chars() {
        match c {
            '\\' if inside && !escaped => escaped = true,
            '"' if !escaped => inside = !inside,
            _ => escaped = false,
        }
    }
    inside
}

/// Whether a new filter can start after `head` (ignoring trailing spaces).
fn expects_filter(head: &str) -> bool {
    let head = head.trim_end();
    if head.is_empty() || head.ends_with(['|', '(', '[', '{', ',', ';', ':']) {
        return true;
    }
    if head.ends_with(['+', '-', '*', '/', '%', '=', '<', '>', '?']) {
        // `.foo?` ends a filter, `?//` is an operator.
        return !head.ends_with('?') || head.ends_with("?//");
    }
    let last_word = head
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default();
    let before = &head[..head.len() - last_word.len()];
    matches!(
        last_word,
        "and" | "or" | "not" | "if" | "then" | "elif" | "else"
    ) && !before.ends_with(['.', '$', '@'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Option<(String, Vec<String>)> {
        complete(text, 1000).map(|(head, items, _)| (head, items))
    }

    #[test]
    fn all_builtins_compile() {
        assert!(!BUILTINS.is_empty());
        for (name, doc) in BUILTINS.iter() {
            let compiles = [0, 1, 2, 3].iter().any(|arity| {
                let args = vec!["."; *arity].join("; ");
                let query = if *arity == 0 {
                    name.clone()
                } else {
                    format!("{name}({args})")
                };
                crate::json::run_jaq(&query, &[], &|| false).is_ok()
            });
            assert!(compiles, "{name} is not supported by jaq");
            assert!(!doc.is_empty(), "{name} has no documentation");
        }
    }

    #[test]
    fn completes_the_identifier_being_typed() {
        let (head, items) = names(".item[0] | sel").unwrap();
        assert_eq!(head, ".item[0] | ");
        assert_eq!(items, vec!["select"]);
        assert_eq!(names(". | select").unwrap().1, vec!["select"]);
        let (head, items) = names("map(to_").unwrap();
        assert_eq!(head, "map(");
        assert!(items.contains(&"to_entries".to_string()));
        assert!(items.iter().all(|n| n.starts_with("to_")));
        assert!(names(". | xyz").unwrap().1.is_empty());
    }

    #[test]
    fn lists_everything_where_a_filter_starts() {
        let all = BUILTINS.len();
        for text in [
            ".a | ",
            ".a |",
            "map(",
            "[",
            ".a, ",
            ".a and ",
            "if . then ",
            ". + ",
        ] {
            assert_eq!(names(text).unwrap().1.len(), all, "{text:?}");
        }
    }

    #[test]
    fn no_functions_in_other_positions() {
        for text in [
            ".fo",
            ".a.sel",
            "$x",
            "@bas",
            ".a ",
            ".a",
            ".[0]",
            ".a?",
            "\"a | sel",
            "12",
        ] {
            assert_eq!(names(text), None, "{text:?}");
        }
        assert!(names(".a?// ").is_some());
    }

    #[test]
    fn finds_the_function_whose_arguments_are_typed() {
        assert_eq!(enclosing_call(". | select("), Some("select"));
        assert_eq!(enclosing_call(". | select(.a == 1 and (.b"), Some("select"));
        assert_eq!(enclosing_call("map(select(.a | [1, "), Some("select"));
        assert_eq!(enclosing_call("map(select(.a)) | map(.b"), Some("map"));
        assert_eq!(enclosing_call("sub(\"(a\"; "), Some("sub"));
        assert_eq!(enclosing_call("test (\"a"), Some("test"));
        assert_eq!(enclosing_call("select(.a)"), None);
        assert_eq!(enclosing_call(".select(.a"), None);
        assert_eq!(enclosing_call("def f(x): x; f("), None);
        assert_eq!(enclosing_call(". | "), None);
    }

    #[test]
    fn documentation_is_available() {
        assert!(doc("select")
            .unwrap()
            .contains("select(boolean_expression)"));
        assert_eq!(doc("nope"), None);
    }
}
