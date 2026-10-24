//! SQL text for types and literal values written in HCL.

use super::names::qualified;
use openmig_hcl::{Address, Number, Value};

/// Where a type is used. Routine argument and result types carry no modifiers in PostgreSQL
/// (`varchar(10)` is stored as `character varying`), so builtin modifiers are dropped there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeContext {
    /// Columns, composite fields, domains, sequences.
    Column,
    /// Function and procedure arguments, results and `RETURNS TABLE` columns.
    Routine,
}

/// Builtin types PostgreSQL prints with a name of its own (`format_type`), by every spelling
/// HCL accepts. The `_` forms stand for the multi-word names, which HCL identifiers cannot hold.
fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "int" | "integer" | "int4" => "integer",
        "smallint" | "int2" => "smallint",
        "bigint" | "int8" => "bigint",
        "real" | "float4" => "real",
        "double_precision" | "float8" | "float" => "double precision",
        "bool" | "boolean" => "boolean",
        "numeric" | "decimal" | "dec" => "numeric",
        "varchar" | "character_varying" => "character varying",
        "char" | "character" => "character",
        "timestamp" | "timestamp_without_time_zone" => "timestamp without time zone",
        "timestamptz" | "timestamp_with_time_zone" => "timestamp with time zone",
        "time" | "time_without_time_zone" => "time without time zone",
        "timetz" | "time_with_time_zone" => "time with time zone",
        "varbit" | "bit_varying" => "bit varying",
        "bit" => "bit",
        "interval" => "interval",
        _ => return None,
    })
}

/// `format_type` text of a builtin with modifiers: `numeric(10,2)`, `timestamp(3) with time
/// zone`. Modifiers are dropped in routine context.
fn builtin_with(canonical: &str, mods: &[String], ctx: TypeContext) -> String {
    if ctx == TypeContext::Routine {
        return canonical.to_owned();
    }
    if mods.is_empty() {
        return match canonical {
            "character" | "bit" => format!("{canonical}(1)"),
            _ => canonical.to_owned(),
        };
    }
    let joined = mods.join(",");
    match canonical {
        "timestamp without time zone" => format!("timestamp({joined}) without time zone"),
        "timestamp with time zone" => format!("timestamp({joined}) with time zone"),
        "time without time zone" => format!("time({joined}) without time zone"),
        "time with time zone" => format!("time({joined}) with time zone"),
        "numeric" if mods.len() == 1 => format!("numeric({joined},0)"),
        _ => format!("{canonical}({joined})"),
    }
}

/// SQL text of a call argument: numbers and identifiers as written, strings as literals.
fn call_arg(value: &Value) -> Result<String, String> {
    match value {
        Value::Number(n) => Ok(n.to_string()),
        Value::Ident(w) => Ok(w.clone()),
        Value::String(s) => Ok(quote_literal(s)),
        other => Err(format!(
            "type arguments must be numbers, identifiers or strings, found {}",
            other.type_name()
        )),
    }
}

/// `float(p)`: `real` up to 24 bits of precision, `double precision` above.
fn float_type(args: &[Value]) -> Option<&'static str> {
    match args {
        [Value::Number(n)] => Some(if n.as_i64()? <= 24 {
            "real"
        } else {
            "double precision"
        }),
        _ => None,
    }
}

/// The SQL text of a type written as an identifier, call or string. References are resolved by
/// the caller (see [`ref_type`]). Builtin spellings are normalized to the names PostgreSQL prints
/// (`timestamptz` becomes `timestamp with time zone`, `varchar(255)` becomes `character
/// varying(255)`); anything else is kept as written.
pub(crate) fn sql_type(value: &Value, ctx: TypeContext) -> Result<String, String> {
    match value {
        Value::Ident(word) => {
            let lower = word.to_ascii_lowercase();
            Ok(match builtin(&lower) {
                Some(canonical) => builtin_with(canonical, &[], ctx),
                None => lower,
            })
        }
        Value::Call { name, args } => {
            let lower = name.to_ascii_lowercase();
            if lower == "float"
                && let Some(t) = float_type(args)
            {
                return Ok(t.to_owned());
            }
            let mods = args.iter().map(call_arg).collect::<Result<Vec<_>, _>>()?;
            Ok(match builtin(&lower) {
                Some(canonical) => builtin_with(canonical, &mods, ctx),
                None => format!("{lower}({})", mods.join(", ")),
            })
        }
        Value::String(s) if !s.trim().is_empty() => Ok(s.clone()),
        other => Err(format!(
            "a type must be an identifier, a call, a string or a type reference, found {}",
            other.type_name()
        )),
    }
}

/// Block kinds a type reference may point to.
pub(crate) const TYPE_KINDS: &[&str] = &["enum", "composite", "domain", "table", "view"];

/// SQL text of a reference to a type-defining object named `schema.name`.
pub(crate) fn ref_type(schema: &str, name: &str) -> String {
    qualified(schema, name)
}

/// Whether `address` can name a type.
pub(crate) fn is_type_ref(address: &Address) -> bool {
    address.nested.is_empty() && TYPE_KINDS.contains(&address.kind.as_str())
}

/// `'text'` with embedded quotes doubled.
pub(crate) fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The type without its modifiers, as PostgreSQL prints the target of a literal's cast:
/// `character varying(10)` becomes `character varying`, `numeric(12,2)[]` becomes `numeric[]`.
pub(crate) fn cast_type(data_type: &str) -> String {
    let mut out = String::with_capacity(data_type.len());
    let mut depth = 0usize;
    let mut quoted = false;
    for c in data_type.chars() {
        match c {
            '"' if depth == 0 => {
                quoted = !quoted;
                out.push(c);
            }
            '(' if !quoted => depth += 1,
            ')' if !quoted && depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// SQL text of a literal value for `default`: strings become literals cast to `data_type`
/// (as PostgreSQL prints string defaults), numbers and bools stay as written, `sql()` is raw.
/// `Ok(None)` for `null`.
pub(crate) fn default_sql(value: &Value, data_type: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(format!(
            "{}::{}",
            quote_literal(s),
            cast_type(data_type)
        ))),
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::Sql(s) => Ok(Some(s.clone())),
        other => Err(format!(
            "a default must be a string, number, bool, null or sql(\"...\"), found {}",
            other.type_name()
        )),
    }
}

/// Text form of a scalar value (data rows, storage parameters, configuration values).
pub(crate) fn scalar_text(value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::Ident(w) => Ok(Some(w.clone())),
        other => Err(format!(
            "expected a string, number or bool, found {}",
            other.type_name()
        )),
    }
}

/// The number written for `text` if it reads back as exactly `text`.
pub(crate) fn number_literal(text: &str) -> Option<Number> {
    Number::from_literal(text).filter(|n| n.to_string() == text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn ident(s: &str) -> Value {
        Value::Ident(s.into())
    }

    fn call(name: &str, args: &[i64]) -> Value {
        Value::Call {
            name: name.into(),
            args: args.iter().map(|a| Value::Number((*a).into())).collect(),
        }
    }

    #[test]
    fn builtin_types() {
        let col = |v: &Value| sql_type(v, TypeContext::Column).unwrap();
        let routine = |v: &Value| sql_type(v, TypeContext::Routine).unwrap();
        assert_eq!(col(&ident("text")), "text");
        assert_eq!(col(&ident("int8")), "bigint");
        assert_eq!(col(&ident("timestamptz")), "timestamp with time zone");
        assert_eq!(col(&ident("double_precision")), "double precision");
        assert_eq!(col(&ident("character_varying")), "character varying");
        assert_eq!(col(&ident("UUID")), "uuid");
        assert_eq!(col(&ident("char")), "character(1)");
        assert_eq!(routine(&ident("char")), "character");
        assert_eq!(col(&call("varchar", &[255])), "character varying(255)");
        assert_eq!(routine(&call("varchar", &[255])), "character varying");
        assert_eq!(col(&call("numeric", &[10, 2])), "numeric(10,2)");
        assert_eq!(col(&call("decimal", &[10])), "numeric(10,0)");
        assert_eq!(
            col(&call("timestamp", &[3])),
            "timestamp(3) without time zone"
        );
        assert_eq!(
            col(&call("timestamptz", &[3])),
            "timestamp(3) with time zone"
        );
        assert_eq!(col(&call("float", &[10])), "real");
        assert_eq!(col(&call("float", &[53])), "double precision");
        assert_eq!(col(&call("vector", &[3])), "vector(3)");
        assert_eq!(col(&Value::String("text[]".into())), "text[]");
        let geometry = Value::Call {
            name: "geometry".into(),
            args: vec![ident("Point"), Value::Number(4326.into())],
        };
        assert_eq!(col(&geometry), "geometry(Point, 4326)");
        assert!(sql_type(&Value::Bool(true), TypeContext::Column).is_err());
    }

    #[test]
    fn casts_and_defaults() {
        assert_eq!(cast_type("character varying(100)"), "character varying");
        assert_eq!(
            cast_type("timestamp(3) with time zone"),
            "timestamp with time zone"
        );
        assert_eq!(cast_type("numeric(12,2)[]"), "numeric[]");
        assert_eq!(cast_type("app.\"t(1)\""), "app.\"t(1)\"");
        let d = |v: Value, t: &str| default_sql(&v, t).unwrap();
        assert_eq!(
            d(Value::String("it's".into()), "text"),
            Some("'it''s'::text".into())
        );
        assert_eq!(d(Value::Number(0.into()), "integer"), Some("0".into()));
        assert_eq!(d(Value::Bool(false), "boolean"), Some("false".into()));
        assert_eq!(
            d(Value::Sql("now()".into()), "timestamp"),
            Some("now()".into())
        );
        assert_eq!(d(Value::Null, "text"), None);
        assert!(number_literal("007").is_none());
        assert!(number_literal("1.50").is_some());
    }
}
