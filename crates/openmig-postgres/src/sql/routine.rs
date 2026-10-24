//! Functions and procedures.

use super::{ident, literal, qualified, routine_name};
use crate::model::{ArgMode, Function, FunctionBody, Returns};
use openmig_core::sql::dollar_tag;
use std::collections::BTreeMap;

/// Settings whose value is a list of names (`GUC_LIST_QUOTE` in PostgreSQL): each element is
/// written as its own literal.
const LIST_SETTINGS: &[&str] = &[
    "search_path",
    "temp_tablespaces",
    "session_preload_libraries",
    "shared_preload_libraries",
    "local_preload_libraries",
];

fn args(f: &Function) -> String {
    f.args
        .iter()
        .map(|a| {
            let mut s = String::new();
            if a.mode != ArgMode::In {
                s.push_str(a.mode.as_sql());
                s.push(' ');
            }
            if let Some(n) = &a.name {
                s.push_str(&ident(n));
                s.push(' ');
            }
            s.push_str(&a.data_type);
            if let Some(d) = &a.default {
                s.push_str(&format!(" DEFAULT {d}"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn returns(r: &Returns) -> String {
    match r {
        Returns::Type { data_type, setof } => {
            format!("RETURNS {}{data_type}", if *setof { "SETOF " } else { "" })
        }
        Returns::Table(columns) => {
            let cols: Vec<String> = columns
                .iter()
                .map(|c| format!("{} {}", ident(&c.name), c.data_type))
                .collect();
            format!("RETURNS TABLE ({})", cols.join(", "))
        }
    }
}

/// Whether the function returns a set (`ROWS` applies).
fn returns_set(f: &Function) -> bool {
    match &f.returns {
        Some(Returns::Type { setof, .. }) => *setof,
        Some(Returns::Table(_)) => true,
        None => false,
    }
}

/// `SET name = value` clauses (`proconfig`).
pub(crate) fn config_clause(name: &str, value: &str) -> String {
    if LIST_SETTINGS.contains(&name.to_ascii_lowercase().as_str()) {
        let items: Vec<String> = split_list_setting(value)
            .iter()
            .map(|v| literal(v))
            .collect();
        format!("SET {name} TO {}", items.join(", "))
    } else {
        format!("SET {name} TO {}", literal(value))
    }
}

/// Split a list setting as PostgreSQL stores it (`a, "B c"`) into its elements.
fn split_list_setting(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                current.push('"');
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut current).trim().to_owned()),
            c => current.push(c),
        }
    }
    if !value.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// Attribute clauses after the signature (everything except the body).
fn attributes(f: &Function, procedure: bool) -> String {
    let mut parts = vec![format!("LANGUAGE {}", ident_or_word(&f.language))];
    if !procedure {
        if f.window {
            parts.push("WINDOW".into());
        }
        parts.push(f.volatility.as_sql().into());
        if f.leakproof {
            parts.push("LEAKPROOF".into());
        }
        parts.push(
            if f.strict {
                "STRICT"
            } else {
                "CALLED ON NULL INPUT"
            }
            .into(),
        );
    }
    parts.push(
        if f.security_definer {
            "SECURITY DEFINER"
        } else {
            "SECURITY INVOKER"
        }
        .into(),
    );
    if !procedure {
        parts.push(format!("PARALLEL {}", f.parallel.as_sql()));
        if !f.cost.is_empty() {
            parts.push(format!("COST {}", f.cost));
        }
        if returns_set(f) && !f.rows.is_empty() && f.rows != "0" {
            parts.push(format!("ROWS {}", f.rows));
        }
    }
    for (k, v) in &f.config {
        parts.push(config_clause(k, v));
    }
    parts.join(" ")
}

/// Language names are identifiers; the common ones are written bare.
fn ident_or_word(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        name.to_owned()
    } else {
        ident(name)
    }
}

/// `CREATE [OR REPLACE] FUNCTION|PROCEDURE ...`.
pub(crate) fn create_routine(schema: &str, f: &Function, or_replace: bool) -> String {
    let procedure = f.returns.is_none();
    let mut sql = format!(
        "CREATE {}{} {}({})",
        if or_replace { "OR REPLACE " } else { "" },
        if procedure { "PROCEDURE" } else { "FUNCTION" },
        qualified(schema, &f.name),
        args(f)
    );
    if let Some(r) = &f.returns {
        sql.push(' ');
        sql.push_str(&returns(r));
    }
    sql.push(' ');
    sql.push_str(&attributes(f, procedure));
    match &f.body {
        FunctionBody::Definition(src) => {
            let tag = dollar_tag(src);
            sql.push_str(&format!(" AS {tag}{src}{tag}"));
        }
        FunctionBody::Standard(body) => {
            sql.push(' ');
            sql.push_str(body);
        }
    }
    sql
}

pub(crate) fn drop_routine(schema: &str, f: &Function) -> String {
    format!(
        "DROP {} {}",
        if f.returns.is_none() {
            "PROCEDURE"
        } else {
            "FUNCTION"
        },
        routine_name(schema, f)
    )
}

/// `ALTER FUNCTION` for attribute-only changes; `None` when no attribute differs.
pub(crate) fn alter_routine(
    schema: &str,
    current: &Function,
    desired: &Function,
) -> Option<String> {
    let procedure = desired.returns.is_none();
    let mut parts: Vec<String> = Vec::new();
    if !procedure {
        if current.volatility != desired.volatility {
            parts.push(desired.volatility.as_sql().into());
        }
        if current.strict != desired.strict {
            parts.push(
                if desired.strict {
                    "STRICT"
                } else {
                    "CALLED ON NULL INPUT"
                }
                .into(),
            );
        }
        if current.leakproof != desired.leakproof {
            parts.push(
                if desired.leakproof {
                    "LEAKPROOF"
                } else {
                    "NOT LEAKPROOF"
                }
                .into(),
            );
        }
        if current.parallel != desired.parallel {
            parts.push(format!("PARALLEL {}", desired.parallel.as_sql()));
        }
        if current.cost != desired.cost && !desired.cost.is_empty() {
            parts.push(format!("COST {}", desired.cost));
        }
        if current.rows != desired.rows && returns_set(desired) && !desired.rows.is_empty() {
            parts.push(format!("ROWS {}", desired.rows));
        }
    }
    if current.security_definer != desired.security_definer {
        parts.push(
            if desired.security_definer {
                "SECURITY DEFINER"
            } else {
                "SECURITY INVOKER"
            }
            .into(),
        );
    }
    let changed: BTreeMap<&String, &String> = desired
        .config
        .iter()
        .filter(|(k, v)| current.config.get(*k) != Some(*v))
        .collect();
    for (k, v) in changed {
        parts.push(config_clause(k, v));
    }
    for k in current.config.keys() {
        if !desired.config.contains_key(k) {
            parts.push(format!("RESET {k}"));
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "ALTER {} {} {}",
        if procedure { "PROCEDURE" } else { "FUNCTION" },
        routine_name(schema, desired),
        parts.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FunctionArg, Parallel, ReturnColumn, Volatility};

    fn func() -> Function {
        Function {
            name: "add".into(),
            args: vec![
                FunctionArg {
                    name: Some("a".into()),
                    mode: ArgMode::In,
                    data_type: "integer".into(),
                    default: None,
                },
                FunctionArg {
                    name: Some("b".into()),
                    mode: ArgMode::In,
                    data_type: "integer".into(),
                    default: Some("1".into()),
                },
            ],
            returns: Some(Returns::Type {
                data_type: "integer".into(),
                setof: false,
            }),
            language: "sql".into(),
            body: FunctionBody::Definition("SELECT $$a$$ || a + b".into()),
            volatility: Volatility::Immutable,
            strict: true,
            security_definer: false,
            leakproof: false,
            parallel: Parallel::Safe,
            cost: "100".into(),
            rows: "0".into(),
            window: false,
            config: [("search_path".to_owned(), "pg_catalog, \"A b\"".to_owned())].into(),
            comment: None,
            owner: String::new(),
            acl: Default::default(),
        }
    }

    #[test]
    fn create_function() {
        assert_eq!(
            create_routine("app", &func(), true),
            "CREATE OR REPLACE FUNCTION \"app\".\"add\"(\"a\" integer, \"b\" integer DEFAULT 1) \
             RETURNS integer LANGUAGE sql IMMUTABLE STRICT SECURITY INVOKER PARALLEL SAFE COST \
             100 SET search_path TO 'pg_catalog', 'A b' AS $_$SELECT $$a$$ || a + b$_$"
        );
        let mut f = func();
        f.returns = Some(Returns::Table(vec![ReturnColumn {
            name: "x".into(),
            data_type: "text".into(),
        }]));
        f.rows = "50".into();
        f.config.clear();
        f.config.insert("work_mem".into(), "64MB".into());
        f.body = FunctionBody::Standard("RETURN (a + b)".into());
        assert_eq!(
            create_routine("app", &f, false),
            "CREATE FUNCTION \"app\".\"add\"(\"a\" integer, \"b\" integer DEFAULT 1) RETURNS \
             TABLE (\"x\" text) LANGUAGE sql IMMUTABLE STRICT SECURITY INVOKER PARALLEL SAFE \
             COST 100 ROWS 50 SET work_mem TO '64MB' RETURN (a + b)"
        );
        assert_eq!(
            drop_routine("app", &f),
            "DROP FUNCTION \"app\".\"add\"(integer, integer)"
        );
    }

    #[test]
    fn procedures_and_alter() {
        let mut p = func();
        p.returns = None;
        p.args[1].mode = ArgMode::InOut;
        p.config.clear();
        assert_eq!(
            create_routine("app", &p, false),
            "CREATE PROCEDURE \"app\".\"add\"(\"a\" integer, INOUT \"b\" integer DEFAULT 1) \
             LANGUAGE sql SECURITY INVOKER AS $_$SELECT $$a$$ || a + b$_$"
        );
        let cur = func();
        let mut des = func();
        des.volatility = Volatility::Stable;
        des.config.clear();
        des.security_definer = true;
        assert_eq!(
            alter_routine("app", &cur, &des).unwrap(),
            "ALTER FUNCTION \"app\".\"add\"(integer, integer) STABLE SECURITY DEFINER RESET \
             search_path"
        );
        assert_eq!(alter_routine("app", &cur, &cur), None);
    }

    #[test]
    fn list_settings() {
        assert_eq!(
            split_list_setting("a, \"B \"\"c\"\"\""),
            vec!["a", "B \"c\""]
        );
        assert_eq!(split_list_setting("\"\""), vec![""]);
        assert_eq!(
            config_clause("search_path", "\"\""),
            "SET search_path TO ''"
        );
    }
}
