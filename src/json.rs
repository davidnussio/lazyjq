use jaq_core::{
    compile::Undefined,
    load::{self, lex, Arena, File, Loader},
    Compiler, Ctx, RcIter,
};
use jaq_json::Val;
use std::{collections::BTreeSet, rc::Rc};

use serde_json::{Deserializer, Value};

/// Deserialize JSON string into a vector of serde_json::Value.
/// If max_streams is given, only deserialize up to that many JSON values.
pub fn deserialize(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let deserializer: serde_json::StreamDeserializer<'_, serde_json::de::StrRead<'_>, Value> =
        Deserializer::from_str(json_str).into_iter::<serde_json::Value>();
    let results = match max_streams {
        Some(l) => deserializer.take(l).collect::<Result<Vec<_>, _>>(),
        None => deserializer.collect::<Result<Vec<_>, _>>(),
    };
    results.map_err(anyhow::Error::from)
}

/// Convert a `serde_json::Value` into jaq's value type without cloning it first.
///
/// Mirrors `impl From<serde_json::Value> for Val` from jaq-json (integers that fit
/// into `isize` become `Int`, every other number keeps its textual form in `Num`),
/// but avoids the intermediate deep clone and the per-number string round trip.
pub fn to_val(value: &Value) -> Val {
    match value {
        Value::Null => Val::Null,
        Value::Bool(b) => Val::Bool(*b),
        Value::Number(n) => match n.as_i64().and_then(|i| isize::try_from(i).ok()) {
            Some(i) => Val::Int(i),
            None => Val::Num(Rc::new(n.to_string())),
        },
        Value::String(s) => Val::from(s.clone()),
        Value::Array(a) => a.iter().map(to_val).collect(),
        Value::Object(o) => Val::obj(
            o.iter()
                .map(|(k, v)| (Rc::new(k.clone()), to_val(v)))
                .collect(),
        ),
    }
}

/// Run `query` over `inputs`.
///
/// `is_cancelled` is polled between outputs; when it returns true the evaluation
/// stops early and `Ok(None)` is returned.
pub fn run_jaq(
    query: &str,
    inputs: &[Val],
    is_cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<Value>>> {
    let filter = compile(query)?;
    let mut ret = Vec::<Value>::new();

    let mut outputs = 0usize;

    for input in inputs {
        if is_cancelled() {
            return Ok(None);
        }
        let empty = RcIter::new(core::iter::empty());
        for item in filter.run((Ctx::new([], &empty), input.clone())) {
            match item {
                Ok(val) => ret.push(val.into()),
                Err(err) => return Err(anyhow::anyhow!("{err}")),
            }
            outputs += 1;
            if outputs & 1023 == 0 && is_cancelled() {
                return Ok(None);
            }
        }
    }

    Ok(Some(ret))
}

/// Run `query` over `inputs` and collect the distinct scalar outputs
/// (strings, numbers, booleans, null) encoded as JSON, sorted.
/// Runtime errors are skipped so that heterogeneous inputs still yield values.
/// Collection stops once `limit` distinct values have been found.
pub fn collect_scalar_values(
    query: &str,
    inputs: &[Val],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let filter = compile(query)?;
    let mut values = BTreeSet::new();

    'outer: for input in inputs {
        let empty = RcIter::new(core::iter::empty());
        for val in filter.run((Ctx::new([], &empty), input.clone())).flatten() {
            if matches!(val, Val::Arr(_) | Val::Obj(_)) {
                continue;
            }
            values.insert(Value::from(val).to_string());
            if values.len() >= limit {
                break 'outer;
            }
        }
    }

    Ok(values.into_iter().collect())
}

fn compile(query: &str) -> anyhow::Result<jaq_core::Filter<jaq_core::Native<Val>>> {
    let arena = Arena::default();
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let modules = loader
        .load(
            &arena,
            File {
                code: query,
                path: (),
            },
        )
        .map_err(|errs| anyhow::anyhow!(describe_load_errors(errs)))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| anyhow::anyhow!(describe_compile_errors(errs)))?;
    Ok(filter)
}

/// Turn jaq's parse errors into a short, single-line, human readable message
/// (e.g. "missing closing parenthesis", "expected term, found `)`").
fn describe_load_errors(errs: load::Errors<&str, ()>) -> String {
    let messages = errs
        .into_iter()
        .flat_map(|(_file, err)| match err {
            load::Error::Io(errs) => errs
                .into_iter()
                .map(|(path, e)| format!("cannot load `{path}`: {e}"))
                .collect::<Vec<_>>(),
            load::Error::Lex(errs) => errs
                .into_iter()
                .map(|(expect, found)| {
                    let what = match expect {
                        lex::Expect::Delim("(") => return "missing closing parenthesis".into(),
                        lex::Expect::Delim("[") => return "missing closing bracket".into(),
                        lex::Expect::Delim("{") => return "missing closing brace".into(),
                        lex::Expect::Delim("\"") => return "missing closing quote".into(),
                        lex::Expect::Delim(_) => "closing delimiter",
                        other => other.as_str(),
                    };
                    expected(what, found)
                })
                .collect(),
            load::Error::Parse(errs) => errs
                .into_iter()
                .map(|(expect, found)| expected(expect.as_str(), found))
                .collect(),
        })
        .collect::<Vec<_>>();
    format!("syntax error: {}", messages.join("; "))
}

fn describe_compile_errors(errs: jaq_core::compile::Errors<&str, ()>) -> String {
    errs.into_iter()
        .flat_map(|(_file, errs)| errs)
        .map(|(name, undefined)| match undefined {
            Undefined::Filter(arity) => format!("undefined filter `{name}/{arity}`"),
            other => format!("undefined {} `{name}`", other.as_str()),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn expected(what: &str, found: &str) -> String {
    let token: String = found
        .split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .take(20)
        .collect();
    if token.is_empty() {
        format!("expected {what} at end of filter")
    } else {
        format!("expected {what}, found `{token}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_completion;

    fn input() -> Vec<serde_json::Value> {
        deserialize(
            r#"{"users":[{"name":"Bob","age":30,"active":true,"tags":["a"]},
                         {"name":"Alice","age":25,"active":false},
                         {"name":"Alice","age":41},
                         {"id":3}],
                "items":[{"price":1.5},{"price":10}]}"#,
            None,
        )
        .unwrap()
    }

    fn vals(stream: &[Value]) -> Vec<Val> {
        stream.iter().map(to_val).collect()
    }

    fn run(query: &str, stream: &[Value]) -> anyhow::Result<Vec<Value>> {
        run_jaq(query, &vals(stream), &|| false).map(Option::unwrap)
    }

    fn values_for(query: &str) -> Vec<String> {
        let req = value_completion::parse(query).expect("value position");
        collect_scalar_values(&req.source_query, &vals(&input()), 100)
            .unwrap()
            .into_iter()
            .filter(|v| req.accepts(v))
            .collect()
    }

    #[test]
    fn values_inside_select() {
        assert_eq!(
            values_for(r#".users[] | select(.name == ""#),
            vec![r#""Alice""#, r#""Bob""#]
        );
        assert_eq!(
            values_for(r#".users[] | select(.name == "A"#),
            vec![r#""Alice""#]
        );
        assert_eq!(
            values_for(".users[] | select(.active == "),
            vec!["false", "null", "true"]
        );
    }

    #[test]
    fn values_inside_map_and_bindings() {
        assert_eq!(
            values_for(".items | map(select(.price > "),
            vec!["1.5", "10"]
        );
        assert_eq!(
            values_for(r#".users[] as $u | select($u.name != "B"#),
            vec![r#""Bob""#]
        );
    }

    #[test]
    fn errors_are_skipped() {
        // `1 | ascii_downcase` fails, but the remaining outputs are still collected.
        let stream = deserialize(r#"[1, "B", "a"]"#, None).unwrap();
        let values = collect_scalar_values(".[] | ascii_downcase", &vals(&stream), 100).unwrap();
        assert_eq!(values, vec![r#""a""#, r#""b""#]);
    }

    #[test]
    fn invalid_query_keeps_error() {
        assert!(run(r#".users[] | select(.name == "Al"#, &input()).is_err());
        assert!(run(r#".users[] | select(.name == "Al")"#, &input())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn to_val_matches_jaq_conversion() {
        let stream = deserialize(
            r#"[0, -7, 1.5, 1e2, 18446744073709551615, "s", true, null, {"k": [1, {"x": 2.0}]}]"#,
            None,
        )
        .unwrap();
        for v in &stream {
            assert_eq!(to_val(v), Val::from(v.clone()), "{v}");
        }
    }

    #[test]
    fn cancellation_stops_evaluation() {
        let stream = deserialize("[1,2,3] [4]", None).unwrap();
        assert_eq!(run_jaq(".", &vals(&stream), &|| true).unwrap(), None);
        assert_eq!(run(".[]", &stream).unwrap().len(), 4);
    }

    #[test]
    fn concise_error_messages() {
        let err = |q: &str| run(q, &input()).unwrap_err().to_string();
        assert_eq!(
            err(".users[] | select(.name == "),
            "syntax error: missing closing parenthesis"
        );
        assert_eq!(
            err(".users[] |"),
            "syntax error: expected term at end of filter"
        );
        assert_eq!(
            err(".users[] | select(.name"),
            "syntax error: missing closing parenthesis"
        );
        assert_eq!(
            err(r#".name == "Al"#),
            "syntax error: missing closing quote"
        );
        assert_eq!(
            err(r#"select(.a == "A"Al)"#),
            "syntax error: expected semicolon or right parenthesis, found `Al`"
        );
        assert_eq!(err(".users | foo"), "undefined filter `foo/0`");
        assert!(!err(".users | foo").contains('\n'));
    }
}
