//! Values written in the form that decodes back to the inspected SQL text.

use crate::decode::values::{literal, sql_text};
use openmig_core::Dialect;
use openmig_core::sql::{TokenKind, tokenize};
use openmig_hcl::write::WExpr;
use openmig_hcl::{Number, Value};

/// Function names the HCL loader evaluates itself; calls with these names cannot be written.
const HCL_FUNCTIONS: &[&str] = &[
    "chomp",
    "coalesce",
    "concat",
    "contains",
    "distinct",
    "file",
    "flatten",
    "format",
    "join",
    "jsondecode",
    "jsonencode",
    "keys",
    "length",
    "lower",
    "merge",
    "range",
    "replace",
    "split",
    "sql",
    "tobool",
    "tonumber",
    "tostring",
    "trimprefix",
    "trimspace",
    "trimsuffix",
    "try",
    "upper",
    "values",
];

/// Identifiers that HCL would read as something else: literals, traversal roots and block kinds
/// (which start references).
const RESERVED: &[&str] = &[
    "true",
    "false",
    "null",
    "var",
    "local",
    "each",
    "database",
    "table",
    "view",
    "materialized_view",
    "dictionary",
    "function",
    "role",
    "grant",
    "row_policy",
    "data",
    "column",
    "index",
    "projection",
    "constraint",
    "attribute",
];

/// Converts a written expression into the value the loader produces for it.
pub fn to_value(expr: &WExpr) -> Option<Value> {
    Some(match expr {
        WExpr::Null => Value::Null,
        WExpr::Bool(b) => Value::Bool(*b),
        WExpr::Number(n) => Value::Number(Number::from_literal(n)?),
        WExpr::String(s) => Value::String(s.clone()),
        WExpr::Ident(w) => Value::Ident(w.clone()),
        WExpr::Sql(s) => Value::Sql(s.clone()),
        WExpr::Call(name, args) => Value::Call {
            name: name.clone(),
            args: args.iter().map(to_value).collect::<Option<_>>()?,
        },
        WExpr::List(items) => Value::List(items.iter().map(to_value).collect::<Option<_>>()?),
        WExpr::Ref(_) | WExpr::Object(_) => return None,
    })
}

/// Whether `word` can be written as a bare identifier and read back as one.
pub fn is_hcl_ident(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !RESERVED.contains(&word)
}

/// An identifier when `word` is one, else a string.
pub fn ident_or_string(word: &str) -> WExpr {
    if is_hcl_ident(word) {
        WExpr::Ident(word.to_owned())
    } else {
        WExpr::String(word.to_owned())
    }
}

/// `name`, `name(arg, ...)` with string, number, identifier and nested call arguments.
fn call_form(text: &str) -> Option<WExpr> {
    let tokens: Vec<_> = tokenize(text, Dialect::ClickHouse)
        .ok()?
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .collect();
    let mut pos = 0;
    let expr = parse_call(&tokens, &mut pos)?;
    (pos == tokens.len()).then_some(expr)
}

fn parse_call(tokens: &[openmig_core::sql::Token<'_>], pos: &mut usize) -> Option<WExpr> {
    let word = tokens.get(*pos).filter(|t| t.kind == TokenKind::Word)?.text;
    if !is_hcl_ident(word) {
        return None;
    }
    *pos += 1;
    if tokens.get(*pos).is_none_or(|t| t.text != "(") {
        return Some(WExpr::Ident(word.to_owned()));
    }
    if HCL_FUNCTIONS.contains(&word) {
        return None;
    }
    *pos += 1;
    let mut args = Vec::new();
    if tokens.get(*pos).is_some_and(|t| t.text == ")") {
        *pos += 1;
        return Some(WExpr::Call(word.to_owned(), args));
    }
    loop {
        let token = tokens.get(*pos)?;
        let arg = match token.kind {
            TokenKind::String => {
                *pos += 1;
                WExpr::String(crate::ddl::unquote_string(token.text)?)
            }
            TokenKind::Number => {
                *pos += 1;
                Number::from_literal(token.text)?;
                WExpr::Number(token.text.to_owned())
            }
            TokenKind::Word => parse_call(tokens, pos)?,
            _ => return None,
        };
        args.push(arg);
        match tokens.get(*pos)?.text {
            "," => *pos += 1,
            ")" => {
                *pos += 1;
                return Some(WExpr::Call(word.to_owned(), args));
            }
            _ => return None,
        }
    }
}

/// Types, engines, index types, codecs: an identifier or call when it decodes back to `text`,
/// else the text as a string.
pub fn sql_text_expr(text: &str) -> WExpr {
    if let Some(expr) = call_form(text)
        && to_value(&expr).is_some_and(|v| sql_text(&v).ok().as_deref() == Some(text))
    {
        return expr;
    }
    WExpr::String(text.to_owned())
}

/// A literal (`default`, setting values): a bool, number or string when that decodes back to
/// `text`, else `sql("...")`.
pub fn literal_expr(text: &str) -> WExpr {
    let mut candidates = Vec::new();
    if text == "true" || text == "false" {
        candidates.push(WExpr::Bool(text == "true"));
    }
    if Number::from_literal(text).is_some() {
        candidates.push(WExpr::Number(text.to_owned()));
    }
    if let Some(s) = crate::ddl::unquote_string(text) {
        candidates.push(WExpr::String(s));
    }
    for candidate in candidates {
        if to_value(&candidate).is_some_and(|v| literal(&v).ok().as_deref() == Some(text)) {
            return candidate;
        }
    }
    WExpr::Sql(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_and_strings() {
        assert_eq!(
            sql_text_expr(
                "ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)"
            ),
            WExpr::Call(
                "ReplicatedMergeTree".into(),
                vec![
                    WExpr::String("/clickhouse/tables/{uuid}/{shard}".into()),
                    WExpr::String("{replica}".into()),
                    WExpr::Ident("updated_at".into()),
                ]
            )
        );
        assert_eq!(
            sql_text_expr("Map(LowCardinality(String), String)"),
            WExpr::Call(
                "Map".into(),
                vec![
                    WExpr::Call("LowCardinality".into(), vec![WExpr::Ident("String".into())]),
                    WExpr::Ident("String".into()),
                ]
            )
        );
        assert_eq!(sql_text_expr("UInt64"), WExpr::Ident("UInt64".into()));
        assert_eq!(
            sql_text_expr("HASHED()"),
            WExpr::Call("HASHED".into(), vec![])
        );
        assert_eq!(
            sql_text_expr("Enum8('a' = 1, 'b' = 2)"),
            WExpr::String("Enum8('a' = 1, 'b' = 2)".into())
        );
        assert_eq!(
            sql_text_expr("Array(Tuple(word String, count UInt32))"),
            WExpr::String("Array(Tuple(word String, count UInt32))".into())
        );
        assert_eq!(
            sql_text_expr("DateTime64(3,'UTC')"),
            WExpr::String("DateTime64(3,'UTC')".into())
        );
        assert_eq!(
            sql_text_expr("length(x)"),
            WExpr::String("length(x)".into())
        );
    }

    #[test]
    fn literals() {
        assert_eq!(literal_expr("0"), WExpr::Number("0".into()));
        assert_eq!(literal_expr("''"), WExpr::String(String::new()));
        assert_eq!(literal_expr("'it\\'s'"), WExpr::String("it's".into()));
        assert_eq!(literal_expr("now()"), WExpr::Sql("now()".into()));
        assert_eq!(literal_expr("true"), WExpr::Bool(true));
        assert_eq!(literal_expr("1.50"), WExpr::Number("1.50".into()));
        assert_eq!(literal_expr("-1"), WExpr::Number("-1".into()));
    }
}
