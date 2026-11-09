use jaq_core::{
    load::{Arena, File, Loader},
    Compiler, Ctx, RcIter,
};
use jaq_json::Val;

use promkit_widgets::{
    jsonstream::jsonz,
    serde_json::{self, Deserializer, Value},
};

/// Get all JSON paths from the input JSON string,
/// respecting the max_streams limit if provided.
pub async fn get_all_paths(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<impl Iterator<Item = String>> {
    let stream = deserialize(json_str, max_streams)?;
    let paths = jsonz::get_all_paths(stream.iter()).collect::<Vec<_>>();
    Ok(paths.into_iter())
}

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

pub fn run_jaq(
    query: &str,
    json_stream: &[serde_json::Value],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let filter = compile(query)?;
    let mut ret = Vec::<serde_json::Value>::new();

    for input in json_stream {
        let inputs = RcIter::new(core::iter::empty());
        let out = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));
        for item in out {
            match item {
                Ok(val) => ret.push(val.into()),
                Err(err) => return Err(anyhow::anyhow!("jq filter execution failed: {err}")),
            }
        }
    }

    Ok(ret)
}

/// Run `query` over `json_stream` and collect the distinct scalar outputs
/// (strings, numbers, booleans, null) encoded as JSON, sorted.
/// Runtime errors are skipped so that heterogeneous inputs still yield values.
/// Collection stops once `limit` distinct values have been found.
pub fn collect_scalar_values(
    query: &str,
    json_stream: &[serde_json::Value],
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let filter = compile(query)?;
    let mut values = std::collections::BTreeSet::new();

    'outer: for input in json_stream {
        let inputs = RcIter::new(core::iter::empty());
        let out = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));
        for val in out.flatten() {
            let val: serde_json::Value = val.into();
            if val.is_array() || val.is_object() {
                continue;
            }
            values.insert(val.to_string());
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
        .map_err(|errs| anyhow::anyhow!("jq filter parsing failed: {errs:?}"))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| anyhow::anyhow!("jq filter compilation failed: {errs:?}"))?;
    Ok(filter)
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

    fn values_for(query: &str) -> Vec<String> {
        let req = value_completion::parse(query).expect("value position");
        collect_scalar_values(&req.source_query, &input(), 100)
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
        let values = collect_scalar_values(".[] | ascii_downcase", &stream, 100).unwrap();
        assert_eq!(values, vec![r#""a""#, r#""b""#]);
    }

    #[test]
    fn invalid_query_keeps_error() {
        assert!(run_jaq(r#".users[] | select(.name == "Al"#, &input()).is_err());
        assert!(run_jaq(r#".users[] | select(.name == "Al")"#, &input())
            .unwrap()
            .is_empty());
    }
}
