//! Context-aware completion of JSON *values* (as opposed to paths).
//!
//! When the query ends with something like
//!
//! ```text
//! .users[] | select(.name == "Al
//! .items | map(select(.price > 1
//! .users[] | select(.email | endswith("
//! ```
//!
//! we can figure out which jq expression produces the left-hand side values
//! (`.users[] | .name`, `.items | .[] | .price`, `.users[] | .email`),
//! evaluate it against the input and offer the distinct scalar values as suggestions.

/// Comparison operators after which a value is expected.
/// Two-character operators must come first so that `<=` is not mistaken for `<`.
const COMPARISON_OPERATORS: [&str; 6] = ["==", "!=", "<=", ">=", "<", ">"];

/// Functions whose first argument is usually a literal string
/// compared against the function input.
const STRING_FUNCTIONS: [&str; 16] = [
    "startswith",
    "endswith",
    "contains",
    "inside",
    "test",
    "match",
    "capture",
    "scan",
    "split",
    "sub",
    "gsub",
    "ltrimstr",
    "rtrimstr",
    "index",
    "rindex",
    "indices",
];

/// A parsed request to complete a JSON value at the end of the query.
#[derive(Debug, PartialEq, Eq)]
pub struct ValueCompletionRequest {
    /// jq filter whose outputs are the candidate values.
    pub source_query: String,
    /// Query text preceding the value; each suggestion is appended to it.
    pub head: String,
    /// The partially typed value (e.g. `"Al` or `tr`). May be empty.
    pub partial: String,
    /// Whether only string values make sense in this position.
    pub strings_only: bool,
}

impl ValueCompletionRequest {
    /// Whether the given JSON-encoded candidate matches the partially typed value.
    pub fn accepts(&self, value_repr: &str) -> bool {
        if self.strings_only && !value_repr.starts_with('"') {
            return false;
        }
        value_repr.starts_with(&self.partial)
    }
}

/// A currently unclosed bracket in the query.
#[derive(Clone, Copy, Debug)]
struct OpenGroup {
    ch: char,
    /// Byte index of the bracket.
    idx: usize,
}

/// Result of scanning the query: the still-open groups and,
/// if the query ends inside a string literal, where that literal starts.
struct Scan {
    open_groups: Vec<OpenGroup>,
    unterminated_string_start: Option<usize>,
}

fn scan(text: &str) -> Scan {
    let mut open_groups: Vec<OpenGroup> = Vec::new();
    let mut string_start = None;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if string_start.is_some() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                string_start = None;
            }
            continue;
        }
        match ch {
            '"' => string_start = Some(idx),
            '(' | '[' | '{' => open_groups.push(OpenGroup { ch, idx }),
            ')' | ']' | '}' => {
                let opener = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if open_groups.last().map(|g| g.ch) == Some(opener) {
                    open_groups.pop();
                }
            }
            _ => {}
        }
    }

    Scan {
        open_groups,
        unterminated_string_start: string_start,
    }
}

/// Byte indices of `|` (pipe, not `|=`) and `;` that are at nesting depth 0
/// within `text[start..end]` and outside string literals.
fn top_level_separators(text: &str, start: usize, end: usize) -> (Option<usize>, Option<usize>) {
    let slice = &text[start..end];
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut last_pipe = None;
    let mut last_semicolon = None;

    let mut chars = slice.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '|' if depth == 0 => {
                if chars.peek().map(|(_, c)| *c) != Some('=') {
                    last_pipe = Some(start + i);
                }
            }
            ';' if depth == 0 => last_semicolon = Some(start + i),
            _ => {}
        }
    }
    (last_pipe, last_semicolon)
}

/// Return the identifier (function name) ending right before byte index `end`,
/// together with its start index.
fn identifier_before(text: &str, end: usize) -> (&str, usize) {
    let head = &text[..end];
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(end);
    (&head[start..], start)
}

/// Build a jq filter that applies `tail` to the inputs flowing into byte position `pos`.
fn source_at(text: &str, open_groups: &[OpenGroup], pos: usize, tail: String) -> String {
    let groups: Vec<OpenGroup> = open_groups
        .iter()
        .copied()
        .filter(|g| g.idx < pos)
        .collect();
    let innermost = groups.last().copied();
    let mut seg_start = innermost.map(|g| g.idx + g.ch.len_utf8()).unwrap_or(0);

    // `;` separates function arguments; each argument receives the same input.
    let (_, semicolon) = top_level_separators(text, seg_start, pos);
    if let Some(sc) = semicolon {
        seg_start = sc + 1;
    }

    // Everything up to (and including) the last top-level pipe in this segment
    // transforms the input before it reaches `pos`. Keeping the raw text also
    // preserves variable bindings such as `.[] as $x |`.
    let tail = match top_level_separators(text, seg_start, pos).0 {
        Some(pipe) => format!("{} {}", text[seg_start..=pipe].trim(), tail),
        None => tail,
    };

    let Some(group) = innermost else {
        return tail;
    };

    if group.ch != '(' {
        // Array/object construction: the input is passed through unchanged.
        return source_at(text, &groups, group.idx, tail);
    }

    let (func, func_start) = identifier_before(text, group.idx);
    let tail = match func {
        "map" | "map_values" => format!(".[] | {tail}"),
        "with_entries" => format!("to_entries[] | {tail}"),
        _ => tail,
    };
    source_at(text, &groups, func_start, tail)
}

/// Walk backwards from `end` over a postfix/path expression such as
/// `.a.b`, `.["x y"][0]`, `$u.name` or `(.a | .b)` and return its start index.
fn expression_start(text: &str, end: usize) -> usize {
    let mut depth = 0i32;
    let mut start = end;
    for (i, ch) in text[..end].char_indices().rev() {
        match ch {
            ')' | ']' => depth += 1,
            '(' | '[' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            c if depth == 0 && (c.is_whitespace() || "|,;=<>!+*/%:{}".contains(c)) => {
                break;
            }
            _ => {}
        }
        start = i;
    }
    start
}

/// Try to interpret the end of `text` as a position where a JSON value is expected.
pub fn parse(text: &str) -> Option<ValueCompletionRequest> {
    let Scan {
        open_groups,
        unterminated_string_start,
    } = scan(text);

    // Locate the partially typed value.
    let partial_start = match unterminated_string_start {
        Some(idx) => idx,
        None => text
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || "-+._".contains(*c))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(text.len()),
    };
    let partial = &text[partial_start..];
    // `.foo` / `$x` are paths or variables, not literal values.
    if partial.starts_with('.') || partial.starts_with('$') {
        return None;
    }

    let head = &text[..partial_start];
    let trimmed = head.trim_end();

    // Case 1: `<lhs> <op> <partial>`
    if let Some(op) = COMPARISON_OPERATORS
        .iter()
        .find(|op| trimmed.ends_with(*op))
    {
        let lhs_end = text[..trimmed.len() - op.len()].trim_end().len();
        let lhs_start = expression_start(text, lhs_end);
        let lhs = text[lhs_start..lhs_end].trim();
        if lhs.is_empty() {
            return None;
        }

        let mut head = head.to_string();
        if partial.is_empty() && !head.ends_with(char::is_whitespace) {
            head.push(' ');
        }

        return Some(ValueCompletionRequest {
            source_query: source_at(text, &open_groups, lhs_start, lhs.to_string()),
            head,
            partial: partial.to_string(),
            strings_only: false,
        });
    }

    // Case 2: first argument of a string function, e.g. `startswith("Al`
    if partial.is_empty() || partial.starts_with('"') {
        if let Some(group) = open_groups.last() {
            let after_paren = &text[group.idx + 1..partial_start];
            if group.ch == '(' && after_paren.trim().is_empty() {
                let (func, func_start) = identifier_before(text, group.idx);
                if STRING_FUNCTIONS.contains(&func) {
                    return Some(ValueCompletionRequest {
                        source_query: source_at(text, &open_groups, func_start, ".".to_string()),
                        head: head.to_string(),
                        partial: partial.to_string(),
                        strings_only: true,
                    });
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> Option<String> {
        parse(text).map(|r| r.source_query)
    }

    #[test]
    fn root_comparison() {
        let req = parse(r#".name == "Al"#).unwrap();
        assert_eq!(req.source_query, ".name");
        assert_eq!(req.head, ".name == ");
        assert_eq!(req.partial, r#""Al"#);
        assert!(!req.strings_only);
    }

    #[test]
    fn inserts_space_after_operator() {
        let req = parse(".age >=").unwrap();
        assert_eq!(req.source_query, ".age");
        assert_eq!(req.head, ".age >= ");
        assert_eq!(req.partial, "");
    }

    #[test]
    fn select_after_pipe() {
        assert_eq!(
            source(r#".users[] | select(.name == "A"#).as_deref(),
            Some(".users[] | .name")
        );
    }

    #[test]
    fn select_inside_map() {
        assert_eq!(
            source(".items | map(select(.price > 1").as_deref(),
            Some(".items | .[] | .price")
        );
    }

    #[test]
    fn nested_pipe_inside_select() {
        assert_eq!(
            source(r#".users[] | select(.profile | .city != ""#).as_deref(),
            Some(".users[] | .profile | .city")
        );
    }

    #[test]
    fn conjunction_inside_select() {
        assert_eq!(
            source(r#".users[] | select(.age > 3 and .name == "#).as_deref(),
            Some(".users[] | .name")
        );
    }

    #[test]
    fn variable_binding() {
        assert_eq!(
            source(r#".[] as $u | select($u.name == ""#).as_deref(),
            Some(".[] as $u | $u.name")
        );
    }

    #[test]
    fn bracket_path_lhs() {
        assert_eq!(
            source(r#".users[] | select(.["full name"] == ""#).as_deref(),
            Some(r#".users[] | .["full name"]"#)
        );
    }

    #[test]
    fn closed_groups_are_skipped() {
        assert_eq!(
            source(r#"[.users[] | select(.a == 1)] | .[] | select(.b == "#).as_deref(),
            Some(r#"[.users[] | select(.a == 1)] | .[] | .b"#)
        );
    }

    #[test]
    fn parens_inside_strings_are_ignored() {
        assert_eq!(
            source(r#".x | select(.name == "a(b"#).as_deref(),
            Some(".x | .name")
        );
    }

    #[test]
    fn string_function_argument() {
        let req = parse(r#".users[] | select(.email | endswith("@ex"#).unwrap();
        assert_eq!(req.source_query, ".users[] | .email | .");
        assert_eq!(req.head, ".users[] | select(.email | endswith(");
        assert_eq!(req.partial, r#""@ex"#);
        assert!(req.strings_only);
        assert!(req.accepts(r#""@example.com""#));
        assert!(!req.accepts("42"));
    }

    #[test]
    fn non_string_partial() {
        let req = parse(".active == tr").unwrap();
        assert_eq!(req.partial, "tr");
        assert!(req.accepts("true"));
        assert!(!req.accepts("false"));
    }

    #[test]
    fn not_a_value_position() {
        assert_eq!(parse(".users[0].na"), None);
        assert_eq!(parse(".a == .b"), None);
        assert_eq!(parse(".a |= "), None);
        assert_eq!(parse(""), None);
    }
}
