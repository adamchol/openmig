//! Variable type expressions and the conversion of given values to the declared type.

use crate::functions::json_to_value;
use crate::value::{Number, Value};
use hcl_edit::expr::Expression;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum VarType {
    Any,
    String,
    Number,
    Bool,
    List(Box<VarType>),
    Set(Box<VarType>),
    Map(Box<VarType>),
    /// Attribute types are not checked: `object({...})` accepts any value.
    Object,
}

impl fmt::Display for VarType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VarType::Any => f.write_str("any"),
            VarType::String => f.write_str("string"),
            VarType::Number => f.write_str("number"),
            VarType::Bool => f.write_str("bool"),
            VarType::List(t) => write!(f, "list({t})"),
            VarType::Set(t) => write!(f, "set({t})"),
            VarType::Map(t) => write!(f, "map({t})"),
            VarType::Object => f.write_str("object"),
        }
    }
}

const EXPECTED_TYPE: &str =
    "expected a type: string, number, bool, any, list(T), set(T), map(T) or object({...})";

pub(super) fn parse_type(expr: &Expression) -> Result<VarType, String> {
    match expr {
        Expression::Variable(name) => match name.as_str() {
            "any" => Ok(VarType::Any),
            "string" => Ok(VarType::String),
            "number" => Ok(VarType::Number),
            "bool" => Ok(VarType::Bool),
            "list" => Ok(VarType::List(Box::new(VarType::Any))),
            "set" => Ok(VarType::Set(Box::new(VarType::Any))),
            "map" => Ok(VarType::Map(Box::new(VarType::Any))),
            "object" => Ok(VarType::Object),
            other => Err(format!("unknown type `{other}`; {EXPECTED_TYPE}")),
        },
        Expression::FuncCall(call) if !call.name.is_namespaced() => {
            let name = call.name.name.as_str();
            let element = || match call.args.iter().collect::<Vec<_>>().as_slice() {
                [arg] => parse_type(arg).map(Box::new),
                _ => Err(format!("`{name}(...)` takes one element type")),
            };
            match name {
                "list" => Ok(VarType::List(element()?)),
                "set" => Ok(VarType::Set(element()?)),
                "map" => Ok(VarType::Map(element()?)),
                "object" => Ok(VarType::Object),
                other => Err(format!("unknown type `{other}(...)`; {EXPECTED_TYPE}")),
            }
        }
        Expression::Parenthesis(inner) => parse_type(inner.inner()),
        Expression::String(_) => Err(format!("a type is written without quotes; {EXPECTED_TYPE}")),
        _ => Err(EXPECTED_TYPE.to_owned()),
    }
}

/// Converts `--var` text to the declared type. Collections and `any` take JSON when the text
/// starts with `[` or `{`; everything else is treated as a string first.
pub(super) fn from_raw(text: &str, ty: &VarType) -> Result<Value, String> {
    let trimmed = text.trim_start();
    let looks_like_json = trimmed.starts_with('[') || trimmed.starts_with('{');
    let value = match ty {
        VarType::String | VarType::Number | VarType::Bool => Value::String(text.to_owned()),
        _ if looks_like_json => {
            let json: serde_json::Value =
                serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
            json_to_value(json)
        }
        _ => Value::String(text.to_owned()),
    };
    convert(value, ty)
}

/// Checks `value` against `ty`, converting between primitive types where unambiguous
/// (`"false"` to a bool, `5` to a string, `"5"` to a number).
pub(super) fn convert(value: Value, ty: &VarType) -> Result<Value, String> {
    let mismatch = |value: &Value| {
        Err(format!(
            "expected {}, found {}",
            describe(ty),
            value.type_name()
        ))
    };
    match (ty, value) {
        (_, Value::Null) => Ok(Value::Null),
        (VarType::Any | VarType::Object, value) => Ok(value),
        (VarType::String, Value::String(s)) => Ok(Value::String(s)),
        (VarType::String, Value::Number(n)) => Ok(Value::String(n.to_string())),
        (VarType::String, Value::Bool(b)) => Ok(Value::String(b.to_string())),
        (VarType::Number, Value::Number(n)) => Ok(Value::Number(n)),
        (VarType::Number, Value::String(s)) => Number::from_literal(s.trim())
            .map(Value::Number)
            .ok_or_else(|| format!("expected a number, found `{s}`")),
        (VarType::Bool, Value::Bool(b)) => Ok(Value::Bool(b)),
        (VarType::Bool, Value::String(s)) => match s.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("expected `true` or `false`, found `{s}`")),
        },
        (VarType::List(element) | VarType::Set(element), Value::List(items)) => {
            let mut out: Vec<Value> = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                let item = convert(item, element).map_err(|e| format!("element {i}: {e}"))?;
                if !matches!(ty, VarType::Set(_)) || !out.contains(&item) {
                    out.push(item);
                }
            }
            Ok(Value::List(out))
        }
        (VarType::Map(element), Value::Object(map)) => map
            .into_iter()
            .map(|(k, v)| {
                let v = convert(v, element).map_err(|e| format!("key `{k}`: {e}"))?;
                Ok((k, v))
            })
            .collect::<Result<_, String>>()
            .map(Value::Object),
        (_, value) => mismatch(&value),
    }
}

fn describe(ty: &VarType) -> String {
    match ty {
        VarType::String => "a string".to_owned(),
        VarType::Number => "a number".to_owned(),
        VarType::Bool => "a bool".to_owned(),
        VarType::List(_) | VarType::Set(_) => format!("a {ty}"),
        VarType::Map(_) => format!("a {ty} (an object)"),
        VarType::Any | VarType::Object => "a value".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    fn ty(src: &str) -> Result<VarType, String> {
        let expr: Expression = src.parse().unwrap();
        parse_type(&expr)
    }

    fn s(v: &str) -> Value {
        Value::String(v.to_owned())
    }

    #[test]
    fn parses_types() {
        assert_eq!(ty("string"), Ok(VarType::String));
        assert_eq!(ty("number"), Ok(VarType::Number));
        assert_eq!(ty("bool"), Ok(VarType::Bool));
        assert_eq!(ty("any"), Ok(VarType::Any));
        assert_eq!(
            ty("list(string)"),
            Ok(VarType::List(Box::new(VarType::String)))
        );
        assert_eq!(
            ty("map(list(number))"),
            Ok(VarType::Map(Box::new(VarType::List(Box::new(
                VarType::Number
            )))))
        );
        assert_eq!(ty("set(any)"), Ok(VarType::Set(Box::new(VarType::Any))));
        assert_eq!(ty("object({ a = string })"), Ok(VarType::Object));
        assert_eq!(
            ty("list(map(bool))").unwrap().to_string(),
            "list(map(bool))"
        );
    }

    #[test]
    fn rejects_bad_types() {
        assert!(ty("strin").unwrap_err().contains("unknown type `strin`"));
        assert!(ty("\"string\"").unwrap_err().contains("without quotes"));
        assert!(
            ty("list(string, number)")
                .unwrap_err()
                .contains("one element type")
        );
        assert!(ty("tuple([string])").unwrap_err().contains("unknown type"));
        assert!(ty("1").is_err());
    }

    #[test]
    fn converts_raw_text() {
        assert_eq!(from_raw("false", &VarType::Bool), Ok(Value::Bool(false)));
        assert_eq!(from_raw("true", &VarType::Bool), Ok(Value::Bool(true)));
        assert!(from_raw("yes", &VarType::Bool).is_err());
        assert_eq!(
            from_raw("42", &VarType::Number),
            Ok(Value::Number(Number::Int(42)))
        );
        assert!(from_raw("4x", &VarType::Number).is_err());
        assert_eq!(from_raw("false", &VarType::String), Ok(s("false")));
        assert_eq!(
            from_raw("[1, 2]", &VarType::Any).unwrap().type_name(),
            "list"
        );
        assert_eq!(from_raw("plain", &VarType::Any), Ok(s("plain")));
        assert_eq!(
            from_raw(
                r#"["a", "b", "a"]"#,
                &VarType::Set(Box::new(VarType::String))
            ),
            Ok(Value::List(vec![s("a"), s("b")]))
        );
        assert_eq!(
            from_raw("[1, 2]", &VarType::List(Box::new(VarType::String))),
            Ok(Value::List(vec![s("1"), s("2")]))
        );
        assert!(
            from_raw("[1,", &VarType::List(Box::new(VarType::Any)))
                .unwrap_err()
                .starts_with("invalid JSON")
        );
        assert!(
            from_raw("a,b", &VarType::List(Box::new(VarType::String)))
                .unwrap_err()
                .contains("expected a list(string), found string")
        );
        let map = from_raw(r#"{"a": 1}"#, &VarType::Map(Box::new(VarType::Number))).unwrap();
        assert_eq!(
            map,
            Value::Object(IndexMap::from([(
                "a".to_owned(),
                Value::Number(Number::Int(1))
            )]))
        );
    }

    #[test]
    fn converts_typed_values() {
        assert_eq!(
            convert(Value::Number(Number::Int(5)), &VarType::String),
            Ok(s("5"))
        );
        assert_eq!(convert(Value::Null, &VarType::Number), Ok(Value::Null));
        assert_eq!(
            convert(Value::Bool(true), &VarType::Number),
            Err("expected a number, found bool".to_owned())
        );
        assert_eq!(
            convert(
                Value::List(vec![s("x")]),
                &VarType::List(Box::new(VarType::Bool))
            ),
            Err("element 0: expected `true` or `false`, found `x`".to_owned())
        );
        assert_eq!(
            convert(Value::Ident("x".into()), &VarType::Any),
            Ok(Value::Ident("x".into()))
        );
        assert!(convert(Value::Ident("x".into()), &VarType::String).is_err());
    }
}
