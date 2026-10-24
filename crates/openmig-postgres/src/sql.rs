//! SQL text for model objects.
//!
//! Every identifier is quoted and every schema object qualified. Types, expressions, collations
//! and operator classes are emitted verbatim: the model stores them as SQL text (as PostgreSQL
//! prints them with an empty `search_path`, or as written in HCL for decoded models).

mod acl;
mod cluster;
mod data;
mod routine;
mod table;
mod types;
mod view;

pub(crate) use acl::{grant, revoke, revoke_grant_option};
pub(crate) use cluster::*;
pub(crate) use data::{delete_row, insert_rows, update_row};
pub(crate) use routine::*;
pub(crate) use table::*;
pub(crate) use types::*;
pub(crate) use view::*;

use crate::model::{Database, Function, Object, QualifiedName};
use openmig_core::sql::{Dialect, quote_ident, quote_literal};
use openmig_core::{ObjectKind, ObjectRef};

/// `"name"`.
pub(crate) fn ident(name: &str) -> String {
    quote_ident(name, Dialect::Postgres)
}

/// `'text'`.
pub(crate) fn literal(value: &str) -> String {
    quote_literal(value, Dialect::Postgres)
}

/// `"schema"."name"`.
pub(crate) fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", ident(schema), ident(name))
}

/// `"a", "b"`.
pub(crate) fn ident_list<S: AsRef<str>>(names: &[S]) -> String {
    names
        .iter()
        .map(|n| ident(n.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A role name in `GRANT`, `REVOKE` and `TO` lists: `PUBLIC` and the session role keywords stay
/// keywords, everything else is quoted.
pub(crate) fn role_spec(name: &str) -> String {
    match name {
        crate::model::PUBLIC => "PUBLIC".to_owned(),
        _ if is_role_keyword(name) => name.to_owned(),
        _ => ident(name),
    }
}

fn is_role_keyword(name: &str) -> bool {
    matches!(name, "CURRENT_USER" | "SESSION_USER" | "CURRENT_ROLE")
}

/// `"schema"."name"(type, type)`: a function or procedure as `DROP`, `COMMENT` and `GRANT`
/// name it.
pub(crate) fn routine_name(schema: &str, f: &Function) -> String {
    let types: Vec<&str> = f.input_types().collect();
    format!("{}({})", qualified(schema, &f.name), types.join(", "))
}

/// Strip the trailing `;` and whitespace that `pg_get_viewdef` prints after a query.
pub(crate) fn query_text(query: &str) -> &str {
    query.trim_end().trim_end_matches(';').trim_end()
}

/// How `COMMENT ON` and messages name an object: `TABLE "app"."users"`,
/// `COLUMN "app"."users"."email"`, `CONSTRAINT "users_pkey" ON "app"."users"`. `None` when the
/// object is not in `model`.
pub(crate) fn object_target(r: &ObjectRef, model: &Database) -> Option<String> {
    use ObjectKind as K;
    let p = &r.path;
    let object = model.get(r)?;
    Some(match (r.kind, p.as_slice()) {
        (K::Schema, [s]) => format!("SCHEMA {}", ident(s)),
        (K::Extension, [e]) => format!("EXTENSION {}", ident(e)),
        (K::Role, [n]) => format!("ROLE {}", ident(n)),
        (K::EventTrigger, [n]) => format!("EVENT TRIGGER {}", ident(n)),
        (K::Table, [s, n]) => format!("TABLE {}", qualified(s, n)),
        (K::View, [s, n]) => format!("VIEW {}", qualified(s, n)),
        (K::MaterializedView, [s, n]) => format!("MATERIALIZED VIEW {}", qualified(s, n)),
        (K::Sequence, [s, n]) => format!("SEQUENCE {}", qualified(s, n)),
        (K::Enum | K::Composite, [s, n]) => format!("TYPE {}", qualified(s, n)),
        (K::Domain, [s, n]) => format!("DOMAIN {}", qualified(s, n)),
        (K::Function, [s, _]) => match object {
            Object::Function(f) => format!("FUNCTION {}", routine_name(s, f)),
            _ => return None,
        },
        (K::Procedure, [s, _]) => match object {
            Object::Procedure(f) => format!("PROCEDURE {}", routine_name(s, f)),
            _ => return None,
        },
        (K::Column, [s, t, c]) => format!("COLUMN {}.{}", qualified(s, t), ident(c)),
        (K::Index, [s, _, i]) => format!("INDEX {}", qualified(s, i)),
        (K::Check, [s, d, c]) if matches!(object, Object::DomainCheck(_)) => {
            format!("CONSTRAINT {} ON DOMAIN {}", ident(c), qualified(s, d))
        }
        (K::PrimaryKey | K::ForeignKey | K::Unique | K::Check | K::Exclusion, [s, t, c]) => {
            format!("CONSTRAINT {} ON {}", ident(c), qualified(s, t))
        }
        (K::Policy, [s, t, n]) => format!("POLICY {} ON {}", ident(n), qualified(s, t)),
        (K::Trigger, [s, t, n]) => format!("TRIGGER {} ON {}", ident(n), qualified(s, t)),
        _ => return None,
    })
}

/// The word statement comments use for a kind of object.
fn kind_word(kind: ObjectKind) -> &'static str {
    use ObjectKind as K;
    match kind {
        K::PrimaryKey => "primary key",
        K::ForeignKey => "foreign key",
        K::Unique => "unique constraint",
        K::Check => "check constraint",
        K::Exclusion => "exclusion constraint",
        K::MaterializedView => "materialized view",
        K::EventTrigger => "event trigger",
        K::Composite => "composite type",
        K::Enum => "enum type",
        K::Data => "table",
        other => other.as_str(),
    }
}

/// How statement comments name an object: `table "app"."users"`,
/// `column "email" of "app"."users"`.
pub(crate) fn noun(r: &ObjectRef) -> String {
    let p = &r.path;
    let kind = kind_word(r.kind);
    match (r.kind, p.as_slice()) {
        (ObjectKind::Function | ObjectKind::Procedure, [s, sig]) => {
            let (name, args) = sig.split_once('(').unwrap_or((sig, ")"));
            format!("{kind} {}({args}", qualified(s, name))
        }
        (_, [n]) => format!("{kind} {}", ident(n)),
        (_, [s, n]) => format!("{kind} {}", qualified(s, n)),
        (_, [s, t, n]) => format!("{kind} {} of {}", ident(n), qualified(s, t)),
        _ => format!("{kind} {}", r.dotted()),
    }
}

/// `COMMENT ON <target> IS '...'` (or `IS NULL`).
pub(crate) fn comment_on(target: &str, comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("COMMENT ON {target} IS {}", literal(c)),
        None => format!("COMMENT ON {target} IS NULL"),
    }
}

/// `"schema"."table"` of a table, view or other relation reference `[schema, relation, ...]`.
pub(crate) fn relation_of(r: &ObjectRef) -> QualifiedName {
    QualifiedName::new(&r.path[0], &r.path[1])
}

/// `WITH (k = v, ...)` storage parameters, or an empty string.
pub(crate) fn with_options(params: &std::collections::BTreeMap<String, String>) -> String {
    if params.is_empty() {
        return String::new();
    }
    format!(" WITH ({})", option_list(params))
}

/// `k = v, ...` for `WITH (...)` and `SET (...)`.
pub(crate) fn option_list(params: &std::collections::BTreeMap<String, String>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k} = {}", option_value(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A storage parameter value: plain numbers and words stay as they are, anything else is quoted.
fn option_value(v: &str) -> String {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if plain { v.to_owned() } else { literal(v) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArgMode, FunctionArg, FunctionBody, Parallel, Volatility};

    #[test]
    fn names() {
        assert_eq!(qualified("app", "Users"), r#""app"."Users""#);
        assert_eq!(ident("we\"ird"), r#""we""ird""#);
        assert_eq!(role_spec("PUBLIC"), "PUBLIC");
        assert_eq!(role_spec("CURRENT_USER"), "CURRENT_USER");
        assert_eq!(role_spec("current_user"), r#""current_user""#);
        assert_eq!(role_spec("web"), r#""web""#);
        assert_eq!(query_text(" SELECT 1;\n"), " SELECT 1");
        assert_eq!(
            noun(&ObjectRef::new(
                ObjectKind::Column,
                ["app", "users", "email"]
            )),
            r#"column "email" of "app"."users""#
        );
        assert_eq!(
            noun(&ObjectRef::new(
                ObjectKind::Function,
                ["app", "f(integer, text)"]
            )),
            r#"function "app"."f"(integer, text)"#
        );
    }

    #[test]
    fn routine_names() {
        let f = Function {
            name: "f".into(),
            args: vec![
                FunctionArg {
                    name: Some("a".into()),
                    mode: ArgMode::In,
                    data_type: "integer".into(),
                    default: None,
                },
                FunctionArg {
                    name: Some("b".into()),
                    mode: ArgMode::Out,
                    data_type: "text".into(),
                    default: None,
                },
            ],
            returns: None,
            language: "sql".into(),
            body: FunctionBody::Definition("SELECT 1".into()),
            volatility: Volatility::Volatile,
            strict: false,
            security_definer: false,
            leakproof: false,
            parallel: Parallel::Unsafe,
            cost: "100".into(),
            rows: "0".into(),
            window: false,
            config: Default::default(),
            comment: None,
            owner: String::new(),
            acl: Default::default(),
        };
        assert_eq!(routine_name("app", &f), r#""app"."f"(integer)"#);
    }

    #[test]
    fn options() {
        let p = [
            ("fillfactor".to_owned(), "70".to_owned()),
            ("x".to_owned(), "a b".to_owned()),
        ]
        .into_iter()
        .collect();
        assert_eq!(with_options(&p), " WITH (fillfactor = 70, x = 'a b')");
    }
}
