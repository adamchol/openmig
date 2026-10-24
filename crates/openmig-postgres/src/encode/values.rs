//! Values written in the form that decodes back to the inspected text.

use super::Encoder;
use crate::decode::names::{split_name, unquote_single};
use crate::decode::table::collation_text;
use crate::decode::types::{TypeContext, default_sql, number_literal, ref_type, sql_type};
use crate::model::{PUBLIC, TriggerEnabled};
use openmig_hcl::write::WExpr;
use openmig_hcl::{Address, Number, Value};

/// Converts the subset of written expressions the type and value checks need.
fn to_value(expr: &WExpr) -> Option<Value> {
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
        WExpr::Ref(_) | WExpr::List(_) | WExpr::Object(_) => return None,
    })
}

/// Short spellings of builtin types with modifiers: `(printed base, printed suffix, written
/// name)`.
const BUILTIN_CALLS: &[(&str, &str, &str)] = &[
    ("character varying", "", "varchar"),
    ("character", "", "char"),
    ("numeric", "", "numeric"),
    ("timestamp", " with time zone", "timestamptz"),
    ("timestamp", " without time zone", "timestamp"),
    ("time", " with time zone", "timetz"),
    ("time", " without time zone", "time"),
    ("bit varying", "", "varbit"),
    ("bit", "", "bit"),
    ("interval", "", "interval"),
];

/// Short spellings of builtin types without modifiers.
const BUILTIN_NAMES: &[(&str, &str)] = &[
    ("double precision", "double_precision"),
    ("timestamp with time zone", "timestamptz"),
    ("timestamp without time zone", "timestamp"),
    ("time with time zone", "timetz"),
    ("time without time zone", "time"),
    ("character varying", "varchar"),
    ("bit varying", "varbit"),
    ("character", "char"),
];

/// Candidate spellings of a builtin type, most readable first.
fn builtin_candidates(data_type: &str) -> Vec<WExpr> {
    let mut out = Vec::new();
    if let Some((_, short)) = BUILTIN_NAMES.iter().find(|(t, _)| *t == data_type) {
        out.push(WExpr::Ident((*short).to_owned()));
    }
    if let Some(open) = data_type.find('(')
        && let Some(close) = data_type[open..].find(')').map(|c| open + c)
    {
        let base = &data_type[..open];
        let suffix = &data_type[close + 1..];
        let mods: Option<Vec<WExpr>> = data_type[open + 1..close]
            .split(',')
            .map(|m| {
                let m = m.trim();
                m.parse::<i64>().ok().map(|_| WExpr::Number(m.to_owned()))
            })
            .collect();
        if let (Some(mods), Some((_, _, name))) = (
            mods,
            BUILTIN_CALLS
                .iter()
                .find(|(b, s, _)| *b == base && *s == suffix),
        ) {
            out.push(WExpr::Call((*name).to_owned(), mods));
        }
    }
    let plain = !data_type.is_empty()
        && data_type
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !data_type.starts_with(|c: char| c.is_ascii_digit());
    if plain {
        out.push(WExpr::Ident(data_type.to_owned()));
    }
    out
}

/// Literal text of `'...'::cast` when it is a single string literal followed by `suffix`.
fn string_literal(text: &str, suffix: &str) -> Option<String> {
    let body = text.strip_prefix('\'')?;
    let mut value = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            if body[i + 1..].starts_with('\'') {
                value.push('\'');
                chars.next();
            } else {
                return (&body[i + 1..] == suffix).then_some(value);
            }
        } else {
            value.push(c);
        }
    }
    None
}

impl Encoder<'_> {
    /// A type: a reference to a declared type, a builtin identifier or call, or the text.
    pub(super) fn type_expr(&self, data_type: &str, ctx: TypeContext) -> WExpr {
        if let Some(parts) = split_name(data_type)
            && let [schema, name] = parts.as_slice()
            && let Some(kind) = self.type_kind(schema, name)
            && ref_type(schema, name) == data_type
        {
            return WExpr::Ref(self.address(kind, schema, name));
        }
        for candidate in builtin_candidates(data_type) {
            if to_value(&candidate)
                .is_some_and(|v| sql_type(&v, ctx).ok().as_deref() == Some(data_type))
            {
                return candidate;
            }
        }
        WExpr::String(data_type.to_owned())
    }

    /// The block kind of a declared object that defines the type `schema.name`.
    fn type_kind(&self, schema: &str, name: &str) -> Option<&'static str> {
        let s = self.model.schemas.get(schema)?;
        if s.enums.contains_key(name) {
            Some("enum")
        } else if s.composites.contains_key(name) {
            Some("composite")
        } else if s.domains.contains_key(name) {
            Some("domain")
        } else if s.tables.contains_key(name) {
            Some("table")
        } else if s.views.contains_key(name) {
            Some("view")
        } else {
            None
        }
    }

    /// A default expression of a column, domain or argument of type `data_type`: a bool, number
    /// or string literal when that decodes to exactly `text`, else `sql("...")`.
    pub(super) fn default_expr(&self, text: &str, data_type: &str) -> WExpr {
        let mut candidates = Vec::new();
        if text == "true" || text == "false" {
            candidates.push(WExpr::Bool(text == "true"));
        }
        if number_literal(text).is_some() && (!text.starts_with('-') || text.parse::<i64>().is_ok())
        {
            candidates.push(WExpr::Number(text.to_owned()));
        }
        let cast = format!("::{}", crate::decode::types::cast_type(data_type));
        if let Some(s) = string_literal(text, &cast) {
            candidates.push(WExpr::String(s));
        }
        for candidate in candidates {
            if to_value(&candidate)
                .is_some_and(|v| default_sql(&v, data_type).ok().flatten().as_deref() == Some(text))
            {
                return candidate;
            }
        }
        WExpr::Sql(text.to_owned())
    }

    /// A collation: its name when that decodes back, else `sql("...")`.
    pub(super) fn collation_expr(&self, text: &str) -> WExpr {
        if let Some(name) = unquote_single(text) {
            let candidate = Value::String(name.clone());
            if collation_text(&candidate).ok().flatten().as_deref() == Some(text) {
                return WExpr::String(name);
            }
        }
        WExpr::Sql(text.to_owned())
    }

    /// A role: a reference when the role is declared, `PUBLIC`, or the name.
    pub(super) fn role_expr(&self, name: &str) -> WExpr {
        if name == PUBLIC {
            WExpr::Ident(PUBLIC.to_owned())
        } else if self.model.roles.contains_key(name) {
            WExpr::Ref(Address::new("role", vec![name.to_owned()]))
        } else {
            WExpr::String(name.to_owned())
        }
    }

    /// A reference to a function by `schema.name` (the overload without arguments when there are
    /// several), else the qualified name.
    pub(super) fn function_expr(&self, schema: &str, name: &str) -> WExpr {
        if let Some(s) = self.model.schemas.get(schema) {
            let overloads: Vec<(&String, &crate::model::Function)> =
                s.functions.iter().filter(|(_, f)| f.name == name).collect();
            let chosen = match overloads.as_slice() {
                [one] => Some(one.0),
                many => many
                    .iter()
                    .find(|(_, f)| f.input_types().next().is_none())
                    .map(|(sig, _)| *sig),
            };
            if let Some(label) =
                chosen.and_then(|sig| self.function_labels.get(&(schema.to_owned(), sig.clone())))
            {
                return WExpr::Ref(self.address("function", schema, label));
            }
        }
        WExpr::String(format!("{schema}.{name}"))
    }

    /// A reference to a column of a table in the model.
    pub(super) fn column_expr(&self, schema: &str, table: &str, column: &str) -> Option<WExpr> {
        let t = self.model.schemas.get(schema)?.tables.get(table)?;
        t.column(column)?;
        Some(WExpr::Ref(
            self.address("table", schema, table).child("column", column),
        ))
    }
}

/// A relative reference to a column of the enclosing table: `column.id`.
pub(super) fn column_ref(name: &str) -> WExpr {
    WExpr::Ref(Address::new("column", vec![name.to_owned()]))
}

pub(super) fn columns_expr(names: &[String]) -> WExpr {
    WExpr::List(names.iter().map(|n| column_ref(n)).collect())
}

pub(super) fn strings(items: &[String]) -> WExpr {
    WExpr::List(items.iter().map(|s| WExpr::String(s.clone())).collect())
}

pub(super) fn ident(word: &str) -> WExpr {
    WExpr::Ident(word.to_owned())
}

/// A keyword written as an identifier (`SET NULL` as `SET_NULL`).
pub(super) fn keyword(sql: &str) -> WExpr {
    WExpr::Ident(sql.replace(' ', "_"))
}

/// A number written verbatim.
pub(super) fn number(n: impl ToString) -> WExpr {
    WExpr::Number(n.to_string())
}

/// An identifier when it is a plain lower-case word, else a string.
pub(super) fn word(text: &str) -> WExpr {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !text.starts_with(|c: char| c.is_ascii_digit());
    if plain {
        WExpr::Ident(text.to_owned())
    } else {
        WExpr::String(text.to_owned())
    }
}

/// Storage parameters or settings: numbers and bools when they read back as the same text.
pub(super) fn options(map: &std::collections::BTreeMap<String, String>) -> WExpr {
    WExpr::Object(map.iter().map(|(k, v)| (k.clone(), scalar(v))).collect())
}

/// A text value as a number or bool when it reads back as the same text, else a string.
pub(super) fn scalar(text: &str) -> WExpr {
    if text == "true" || text == "false" {
        WExpr::Bool(text == "true")
    } else if number_literal(text).is_some()
        && (!text.starts_with('-') || text.parse::<i64>().is_ok())
    {
        WExpr::Number(text.to_owned())
    } else {
        WExpr::String(text.to_owned())
    }
}

/// `enabled` of a trigger or event trigger, `None` for the default.
pub(super) fn enabled(e: TriggerEnabled) -> Option<WExpr> {
    match e {
        TriggerEnabled::Origin => None,
        TriggerEnabled::Disabled => Some(WExpr::Bool(false)),
        TriggerEnabled::Replica => Some(ident("REPLICA")),
        TriggerEnabled::Always => Some(ident("ALWAYS")),
    }
}
