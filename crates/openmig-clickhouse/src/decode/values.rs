//! Rendering HCL values as ClickHouse SQL text.
//!
//! Call-shaped values (`ReplicatedMergeTree("/path", "{replica}")`, `Map(String, UInt64)`,
//! `ZSTD(1)`) become SQL calls: string arguments become single-quoted literals, identifiers stay
//! bare, numbers are written as they were, nested calls recurse. Arguments are separated by
//! `", "`, the way the server prints them, so a call written in HCL renders exactly as the
//! inspected text.

use openmig_core::Dialect;
use openmig_core::sql::quote_literal;
use openmig_hcl::Value;

/// A SQL expression from a call-shaped value. Top-level strings are string literals.
pub fn sql_expr(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Ident(w) => w.clone(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote_literal(s, Dialect::ClickHouse),
        Value::Bool(b) => b.to_string(),
        Value::Null => "NULL".to_owned(),
        Value::Sql(s) => s.clone(),
        Value::Call { name, args } => {
            let args = args.iter().map(sql_expr).collect::<Result<Vec<_>, _>>()?;
            format!("{name}({})", args.join(", "))
        }
        Value::List(items) => {
            let items = items.iter().map(sql_expr).collect::<Result<Vec<_>, _>>()?;
            format!("[{}]", items.join(", "))
        }
        Value::Ref(a) => return Err(format!("`{a}` cannot be used in a SQL expression")),
        Value::Object(_) => return Err("an object cannot be used in a SQL expression".into()),
    })
}

/// SQL text written as an identifier, call or raw text: types, engines, index types, layouts.
/// A top-level string is the SQL text itself (`"Enum8('a' = 1)"`).
pub fn sql_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) | Value::Sql(s) if !s.trim().is_empty() => Ok(s.trim().to_owned()),
        Value::String(_) | Value::Sql(_) => Err("an empty string".into()),
        Value::Ident(_) | Value::Call { .. } => sql_expr(value),
        other => Err(format!(
            "an identifier, a call or a string, found {}",
            other.type_name()
        )),
    }
}

/// A value of a literal attribute (`default`, setting values): strings are string literals,
/// `sql("...")` is raw SQL.
pub fn literal(value: &Value) -> Result<String, String> {
    match value {
        Value::Ref(_) | Value::Object(_) => Err(format!(
            "a literal or sql(\"...\"), found {}",
            value.type_name()
        )),
        _ => sql_expr(value),
    }
}

/// A SQL expression attribute (`materialized`, `ttl`, `as`, ...): strings are SQL text.
pub fn expression(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) | Value::Sql(s) => Ok(s.trim().to_owned()),
        Value::Call { .. } | Value::Ident(_) | Value::Number(_) => sql_expr(value),
        other => Err(format!("a SQL string, found {}", other.type_name())),
    }
}

/// Codec elements from a list of identifiers or calls, or from a string (`"ZSTD(1)"`,
/// `"CODEC(Delta(8), ZSTD(1))"`).
pub fn codec(value: &Value) -> Result<Vec<String>, String> {
    match value {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Ident(_) | Value::Call { .. } => sql_expr(item),
                Value::String(s) | Value::Sql(s) => Ok(s.trim().to_owned()),
                other => Err(format!(
                    "codec entries must be identifiers or calls, found {}",
                    other.type_name()
                )),
            })
            .collect(),
        Value::String(s) | Value::Sql(s) => {
            let text = s.trim();
            let inner = text
                .strip_prefix("CODEC(")
                .and_then(|t| t.strip_suffix(')'))
                .unwrap_or(text);
            crate::ddl::split_list(inner).map_err(|e| e.message)
        }
        Value::Ident(_) | Value::Call { .. } => Ok(vec![sql_expr(value)?]),
        other => Err(format!(
            "a list of identifiers or calls, found {}",
            other.type_name()
        )),
    }
}

/// A plain identifier that needs no quoting in SQL.
pub fn is_plain_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A column name as a SQL expression: bare when it is a plain identifier, back-quoted otherwise.
pub fn column_expr(name: &str) -> String {
    if is_plain_identifier(name) {
        name.to_owned()
    } else {
        openmig_core::sql::quote_ident(name, Dialect::ClickHouse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmig_hcl::Number;

    fn call(name: &str, args: Vec<Value>) -> Value {
        Value::Call {
            name: name.into(),
            args,
        }
    }

    #[test]
    fn calls_render_like_the_server() {
        let engine = call(
            "ReplicatedMergeTree",
            vec![
                Value::String("/clickhouse/tables/{uuid}/{shard}".into()),
                Value::String("{replica}".into()),
                Value::Ident("updated_at".into()),
            ],
        );
        assert_eq!(
            sql_text(&engine).unwrap(),
            "ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)"
        );
        let map = call(
            "Map",
            vec![
                call("LowCardinality", vec![Value::Ident("String".into())]),
                Value::Ident("String".into()),
            ],
        );
        assert_eq!(
            sql_text(&map).unwrap(),
            "Map(LowCardinality(String), String)"
        );
        assert_eq!(
            sql_text(&call(
                "DateTime64",
                vec![Value::Number(Number::Int(3)), Value::String("UTC".into())]
            ))
            .unwrap(),
            "DateTime64(3, 'UTC')"
        );
        assert_eq!(
            sql_text(&Value::String("Enum8('a' = 1)".into())).unwrap(),
            "Enum8('a' = 1)"
        );
        assert!(sql_text(&Value::Bool(true)).is_err());
    }

    #[test]
    fn literals_and_expressions() {
        assert_eq!(literal(&Value::String("it's".into())).unwrap(), "'it\\'s'");
        assert_eq!(literal(&Value::Number(Number::Int(0))).unwrap(), "0");
        assert_eq!(literal(&Value::Sql("now()".into())).unwrap(), "now()");
        assert_eq!(literal(&call("now", vec![])).unwrap(), "now()");
        assert_eq!(
            expression(&Value::String("toDate(ts)".into())).unwrap(),
            "toDate(ts)"
        );
        assert_eq!(
            codec(&Value::List(vec![
                call("Delta", vec![Value::Number(Number::Int(8))]),
                call("ZSTD", vec![Value::Number(Number::Int(1))]),
            ]))
            .unwrap(),
            ["Delta(8)", "ZSTD(1)"]
        );
        assert_eq!(
            codec(&Value::Sql("CODEC(Delta(8), ZSTD(1))".into())).unwrap(),
            ["Delta(8)", "ZSTD(1)"]
        );
        assert_eq!(column_expr("ts"), "ts");
        assert_eq!(column_expr("Events.Name"), "`Events.Name`");
    }
}
