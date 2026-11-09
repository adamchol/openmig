//! Whether `ALTER COLUMN ... TYPE t USING c::t` can convert a column, so the diff can recreate
//! columns that no cast covers instead of planning a statement PostgreSQL refuses.

use crate::model::Database;
use crate::plan::refs::qualified_names;

/// Whether PostgreSQL has an explicit conversion from `from` (a type of `cur`) to `to` (a type
/// of `des`) on a server of version `major`. Types it cannot know (extension types, ranges,
/// user-defined casts) are assumed convertible, leaving the decision to the server.
pub(crate) fn castable(cur: &Database, des: &Database, major: u32, from: &str, to: &str) -> bool {
    let from = resolve(cur, from);
    let to = resolve(des, to);
    convertible(major, &from, &to)
}

/// A type with domains replaced by their base types and modifiers removed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Type {
    /// A `pg_catalog` type, as `format_type` names it without modifiers.
    Builtin(String),
    Array(Box<Type>),
    /// An enum or composite type of the model.
    Defined(String),
    Unknown,
}

fn resolve(db: &Database, data_type: &str) -> Type {
    resolve_depth(db, data_type.trim(), 0)
}

fn resolve_depth(db: &Database, data_type: &str, depth: usize) -> Type {
    if let Some(elem) = data_type.strip_suffix("[]") {
        return match resolve_depth(db, elem.trim_end(), depth) {
            Type::Unknown => Type::Unknown,
            t => Type::Array(Box::new(t)),
        };
    }
    let names = qualified_names(data_type);
    let Some((schema, name)) = names.first() else {
        return Type::Builtin(builtin_name(data_type));
    };
    let Some(s) = db.schemas.get(schema) else {
        return Type::Unknown;
    };
    if let Some(d) = s.domains.get(name)
        && depth < 16
    {
        return resolve_depth(db, d.data_type.trim(), depth + 1);
    }
    if s.enums.contains_key(name) || s.composites.contains_key(name) {
        return Type::Defined(format!("{schema}.{name}"));
    }
    Type::Unknown
}

/// `timestamp(3) with time zone` → `timestamp with time zone`, `interval day to second` →
/// `interval`.
fn builtin_name(data_type: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in data_type.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.starts_with("interval") {
        return "interval".to_owned();
    }
    out
}

/// String types: every type converts to and from them through its text form.
fn is_string(t: &Type) -> bool {
    matches!(t, Type::Builtin(n) if matches!(n.as_str(), "text" | "character varying" | "character" | "name"))
}

fn convertible(major: u32, from: &Type, to: &Type) -> bool {
    if from == to || is_string(from) || is_string(to) {
        return true;
    }
    match (from, to) {
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        (Type::Array(a), Type::Array(b)) => convertible(major, a, b),
        (Type::Builtin(a), Type::Builtin(b)) => builtin_cast(major, a, b),
        _ => false,
    }
}

/// The casts between `pg_catalog` types that [`BUILTIN`] and [`VERSIONED`] list for `major`.
pub fn builtin_casts(major: u32) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<_> = BUILTIN
        .iter()
        .copied()
        .chain(
            VERSIONED
                .iter()
                .filter(|c| (c.2..=c.3).contains(&major))
                .map(|c| (c.0, c.1)),
        )
        .collect();
    out.sort();
    out
}

fn builtin_cast(major: u32, from: &str, to: &str) -> bool {
    BUILTIN.binary_search(&(from, to)).is_ok()
        || VERSIONED
            .iter()
            .any(|&(f, t, min, max)| f == from && t == to && (min..=max).contains(&major))
}

/// Casts between `pg_catalog` types in `pg_cast` on every supported version, sorted.
/// `tests/integration/casts.rs` checks this table against the server.
pub(crate) const BUILTIN: &[(&str, &str)] = &[
    ("\"char\"", "character"),
    ("\"char\"", "character varying"),
    ("\"char\"", "integer"),
    ("\"char\"", "text"),
    ("bigint", "bit"),
    ("bigint", "double precision"),
    ("bigint", "integer"),
    ("bigint", "money"),
    ("bigint", "numeric"),
    ("bigint", "oid"),
    ("bigint", "real"),
    ("bigint", "regclass"),
    ("bigint", "regcollation"),
    ("bigint", "regconfig"),
    ("bigint", "regdictionary"),
    ("bigint", "regnamespace"),
    ("bigint", "regoper"),
    ("bigint", "regoperator"),
    ("bigint", "regproc"),
    ("bigint", "regprocedure"),
    ("bigint", "regrole"),
    ("bigint", "regtype"),
    ("bigint", "smallint"),
    ("bit", "bigint"),
    ("bit", "bit"),
    ("bit", "bit varying"),
    ("bit", "integer"),
    ("bit varying", "bit"),
    ("bit varying", "bit varying"),
    ("boolean", "character"),
    ("boolean", "character varying"),
    ("boolean", "integer"),
    ("boolean", "text"),
    ("box", "circle"),
    ("box", "lseg"),
    ("box", "point"),
    ("box", "polygon"),
    ("character", "\"char\""),
    ("character", "character"),
    ("character", "character varying"),
    ("character", "name"),
    ("character", "text"),
    ("character", "xml"),
    ("character varying", "\"char\""),
    ("character varying", "character"),
    ("character varying", "character varying"),
    ("character varying", "name"),
    ("character varying", "regclass"),
    ("character varying", "text"),
    ("character varying", "xml"),
    ("cidr", "character"),
    ("cidr", "character varying"),
    ("cidr", "inet"),
    ("cidr", "text"),
    ("circle", "box"),
    ("circle", "point"),
    ("circle", "polygon"),
    ("date", "timestamp with time zone"),
    ("date", "timestamp without time zone"),
    ("daterange", "datemultirange"),
    ("double precision", "bigint"),
    ("double precision", "integer"),
    ("double precision", "numeric"),
    ("double precision", "real"),
    ("double precision", "smallint"),
    ("inet", "character"),
    ("inet", "character varying"),
    ("inet", "cidr"),
    ("inet", "text"),
    ("int4range", "int4multirange"),
    ("int8range", "int8multirange"),
    ("integer", "\"char\""),
    ("integer", "bigint"),
    ("integer", "bit"),
    ("integer", "boolean"),
    ("integer", "double precision"),
    ("integer", "money"),
    ("integer", "numeric"),
    ("integer", "oid"),
    ("integer", "real"),
    ("integer", "regclass"),
    ("integer", "regcollation"),
    ("integer", "regconfig"),
    ("integer", "regdictionary"),
    ("integer", "regnamespace"),
    ("integer", "regoper"),
    ("integer", "regoperator"),
    ("integer", "regproc"),
    ("integer", "regprocedure"),
    ("integer", "regrole"),
    ("integer", "regtype"),
    ("integer", "smallint"),
    ("interval", "interval"),
    ("interval", "time without time zone"),
    ("json", "jsonb"),
    ("jsonb", "bigint"),
    ("jsonb", "boolean"),
    ("jsonb", "double precision"),
    ("jsonb", "integer"),
    ("jsonb", "json"),
    ("jsonb", "numeric"),
    ("jsonb", "real"),
    ("jsonb", "smallint"),
    ("lseg", "point"),
    ("macaddr", "macaddr8"),
    ("macaddr8", "macaddr"),
    ("money", "numeric"),
    ("name", "character"),
    ("name", "character varying"),
    ("name", "text"),
    ("numeric", "bigint"),
    ("numeric", "double precision"),
    ("numeric", "integer"),
    ("numeric", "money"),
    ("numeric", "numeric"),
    ("numeric", "real"),
    ("numeric", "smallint"),
    ("numrange", "nummultirange"),
    ("oid", "bigint"),
    ("oid", "integer"),
    ("oid", "regclass"),
    ("oid", "regcollation"),
    ("oid", "regconfig"),
    ("oid", "regdictionary"),
    ("oid", "regnamespace"),
    ("oid", "regoper"),
    ("oid", "regoperator"),
    ("oid", "regproc"),
    ("oid", "regprocedure"),
    ("oid", "regrole"),
    ("oid", "regtype"),
    ("path", "polygon"),
    ("pg_dependencies", "bytea"),
    ("pg_dependencies", "text"),
    ("pg_mcv_list", "bytea"),
    ("pg_mcv_list", "text"),
    ("pg_ndistinct", "bytea"),
    ("pg_ndistinct", "text"),
    ("pg_node_tree", "text"),
    ("point", "box"),
    ("polygon", "box"),
    ("polygon", "circle"),
    ("polygon", "path"),
    ("polygon", "point"),
    ("real", "bigint"),
    ("real", "double precision"),
    ("real", "integer"),
    ("real", "numeric"),
    ("real", "smallint"),
    ("regclass", "bigint"),
    ("regclass", "integer"),
    ("regclass", "oid"),
    ("regcollation", "bigint"),
    ("regcollation", "integer"),
    ("regcollation", "oid"),
    ("regconfig", "bigint"),
    ("regconfig", "integer"),
    ("regconfig", "oid"),
    ("regdictionary", "bigint"),
    ("regdictionary", "integer"),
    ("regdictionary", "oid"),
    ("regnamespace", "bigint"),
    ("regnamespace", "integer"),
    ("regnamespace", "oid"),
    ("regoper", "bigint"),
    ("regoper", "integer"),
    ("regoper", "oid"),
    ("regoper", "regoperator"),
    ("regoperator", "bigint"),
    ("regoperator", "integer"),
    ("regoperator", "oid"),
    ("regoperator", "regoper"),
    ("regproc", "bigint"),
    ("regproc", "integer"),
    ("regproc", "oid"),
    ("regproc", "regprocedure"),
    ("regprocedure", "bigint"),
    ("regprocedure", "integer"),
    ("regprocedure", "oid"),
    ("regprocedure", "regproc"),
    ("regrole", "bigint"),
    ("regrole", "integer"),
    ("regrole", "oid"),
    ("regtype", "bigint"),
    ("regtype", "integer"),
    ("regtype", "oid"),
    ("smallint", "bigint"),
    ("smallint", "double precision"),
    ("smallint", "integer"),
    ("smallint", "numeric"),
    ("smallint", "oid"),
    ("smallint", "real"),
    ("smallint", "regclass"),
    ("smallint", "regcollation"),
    ("smallint", "regconfig"),
    ("smallint", "regdictionary"),
    ("smallint", "regnamespace"),
    ("smallint", "regoper"),
    ("smallint", "regoperator"),
    ("smallint", "regproc"),
    ("smallint", "regprocedure"),
    ("smallint", "regrole"),
    ("smallint", "regtype"),
    ("text", "\"char\""),
    ("text", "character"),
    ("text", "character varying"),
    ("text", "name"),
    ("text", "regclass"),
    ("text", "xml"),
    ("time with time zone", "time with time zone"),
    ("time with time zone", "time without time zone"),
    ("time without time zone", "interval"),
    ("time without time zone", "time with time zone"),
    ("time without time zone", "time without time zone"),
    ("timestamp with time zone", "date"),
    ("timestamp with time zone", "time with time zone"),
    ("timestamp with time zone", "time without time zone"),
    ("timestamp with time zone", "timestamp with time zone"),
    ("timestamp with time zone", "timestamp without time zone"),
    ("timestamp without time zone", "date"),
    ("timestamp without time zone", "time without time zone"),
    ("timestamp without time zone", "timestamp with time zone"),
    ("timestamp without time zone", "timestamp without time zone"),
    ("tsrange", "tsmultirange"),
    ("tstzrange", "tstzmultirange"),
    ("xid8", "xid"),
    ("xml", "character"),
    ("xml", "character varying"),
    ("xml", "text"),
];

/// Casts that exist only on some versions: source, target, first and last major version.
pub(crate) const VERSIONED: &[(&str, &str, u32, u32)] = &[
    ("bigint", "bytea", 18, u32::MAX),
    ("bytea", "bigint", 18, u32::MAX),
    ("bytea", "integer", 18, u32::MAX),
    ("bytea", "smallint", 18, u32::MAX),
    ("integer", "bytea", 18, u32::MAX),
    ("path", "point", 0, 14),
    ("smallint", "bytea", 18, u32::MAX),
];

#[cfg(test)]
mod tests;
