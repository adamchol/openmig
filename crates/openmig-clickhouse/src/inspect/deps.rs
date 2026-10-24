//! Dependencies between objects of the model.
//!
//! Hard edges come from the server (`system.tables.dependencies_*` for materialized view sources,
//! `loading_dependencies_*` for dictionaries and tables using them) and from definitions
//! (materialized view `TO` targets, `Distributed` and `Buffer` destinations, row policy tables).
//! Soft edges are guessed from identifiers in view queries, refresh clauses and column
//! expressions: names of tables, views and dictionaries (qualified, or unqualified in the
//! object's own database) and of SQL functions.

use super::Snapshot;
use crate::ddl;
use crate::model::{Dependency, Model, engine_name};
use openmig_core::sql::{TokenKind, tokenize};
use openmig_core::{Dialect, ObjectKind, ObjectRef};
use std::collections::BTreeSet;

pub(super) fn assemble(model: &mut Model, snapshot: &Snapshot) {
    let mut edges = Vec::new();
    let mut hard = |from: ObjectRef, to: Option<ObjectRef>| {
        if let Some(to) = to
            && from != to
        {
            edges.push(Dependency {
                from,
                to,
                soft: false,
            });
        }
    };

    for row in &snapshot.tables {
        let Some(this) = relation(model, &row.database, &row.name) else {
            continue;
        };
        for (db, name) in row
            .dependencies_database
            .iter()
            .zip(&row.dependencies_table)
        {
            if let Some(dependant) = relation(model, db, name) {
                hard(dependant, Some(this.clone()));
            }
        }
        for (db, name) in row
            .loading_dependencies_database
            .iter()
            .zip(&row.loading_dependencies_table)
        {
            hard(this.clone(), relation(model, db, name));
        }
    }

    for (d, db) in &model.databases {
        for (name, mv) in &db.materialized_views {
            let from = ObjectRef::new(ObjectKind::MaterializedView, [d, name]);
            if let Some(to) = &mv.to {
                hard(from, relation(model, &to.database, &to.name));
            }
        }
        for (name, table) in &db.tables {
            let from = ObjectRef::new(ObjectKind::Table, [d, name]);
            if let Some((target_db, target)) = destination(&table.engine) {
                let target_db = target_db.unwrap_or_else(|| d.clone());
                hard(from, relation(model, &target_db, &target));
            }
        }
    }
    for id in model.row_policies.keys() {
        hard(id.object_ref(), relation(model, &id.database, &id.table));
    }

    let mut soft = Vec::new();
    for (d, db) in &model.databases {
        for (name, view) in &db.views {
            let from = ObjectRef::new(ObjectKind::View, [d, name]);
            soft.extend(references(model, d, &from, &view.query));
        }
        for (name, mv) in &db.materialized_views {
            let from = ObjectRef::new(ObjectKind::MaterializedView, [d, name]);
            soft.extend(references(model, d, &from, &mv.query));
            if let Some(refresh) = &mv.refresh {
                soft.extend(references(model, d, &from, refresh));
            }
        }
        for (name, table) in &db.tables {
            let from = ObjectRef::new(ObjectKind::Table, [d, name]);
            for column in &table.columns {
                let texts = column
                    .default
                    .iter()
                    .map(|def| def.expression.as_str())
                    .chain(column.ttl.as_deref());
                for text in texts {
                    soft.extend(
                        references(model, d, &from, text)
                            .filter(|dep| dep.to.kind == ObjectKind::Function),
                    );
                }
            }
        }
    }
    edges.extend(soft);
    model.dependencies = edges;
}

/// The reference of the table, view, materialized view or dictionary `database.name`.
fn relation(model: &Model, database: &str, name: &str) -> Option<ObjectRef> {
    let db = model.databases.get(database)?;
    let kind = if db.tables.contains_key(name) {
        ObjectKind::Table
    } else if db.views.contains_key(name) {
        ObjectKind::View
    } else if db.materialized_views.contains_key(name) {
        ObjectKind::MaterializedView
    } else if db.dictionaries.contains_key(name) {
        ObjectKind::Dictionary
    } else {
        return None;
    };
    Some(ObjectRef::new(kind, [database, name]))
}

/// The table a `Distributed` or `Buffer` table writes to, as `(database, table)`.
fn destination(engine: &str) -> Option<(Option<String>, String)> {
    let (database, table) = match engine_name(engine) {
        "Distributed" => (1, 2),
        "Buffer" => (0, 1),
        _ => return None,
    };
    let args = engine
        .strip_prefix(engine_name(engine))?
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let args = ddl::split_list(args).ok()?;
    let name = |arg: &String| ddl::unquote_string(arg).unwrap_or_else(|| ddl::unquote_ident(arg));
    let db = args.get(database).map(name).filter(|d| !d.is_empty());
    Some((db, name(args.get(table)?)))
}

/// Soft edges from `from` to the objects `sql` names.
fn references<'a>(
    model: &'a Model,
    database: &'a str,
    from: &'a ObjectRef,
    sql: &str,
) -> impl Iterator<Item = Dependency> + 'a {
    let mut found = BTreeSet::new();
    let tokens: Vec<_> = tokenize(sql, Dialect::ClickHouse)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .collect();
    let ident = |i: usize| {
        tokens.get(i).and_then(|t| match t.kind {
            TokenKind::Word => Some(t.text.to_owned()),
            TokenKind::QuotedIdent => Some(ddl::unquote_ident(t.text)),
            _ => None,
        })
    };
    let punct = |i: usize, p: &str| {
        tokens
            .get(i)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text == p)
    };
    let mut i = 0;
    while i < tokens.len() {
        let Some(first) = ident(i) else {
            i += 1;
            continue;
        };
        if punct(i + 1, ".")
            && let Some(second) = ident(i + 2)
        {
            if let Some(to) = relation(model, &first, &second) {
                found.insert(to);
            }
            i += 3;
            continue;
        }
        if punct(i + 1, "(") {
            if model.functions.contains_key(&first) {
                found.insert(ObjectRef::new(ObjectKind::Function, [first.as_str()]));
            }
        } else if !punct(i.wrapping_sub(1), ".")
            && let Some(to) = relation(model, database, &first)
        {
            found.insert(to);
        }
        i += 1;
    }
    found
        .into_iter()
        .filter(move |to| to != from)
        .map(move |to| Dependency {
            from: from.clone(),
            to,
            soft: true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Database, Function, Table, View};

    #[test]
    fn destinations() {
        assert_eq!(
            destination("Distributed('{cluster}', 'app', 'events_local', rand())"),
            Some((Some("app".into()), "events_local".into()))
        );
        assert_eq!(
            destination("Buffer('app', 'events', 1, 10, 100, 10000, 1000000, 10000000, 100000000)"),
            Some((Some("app".into()), "events".into()))
        );
        assert_eq!(
            destination("Distributed(c, '', t)"),
            Some((None, "t".into()))
        );
        assert_eq!(destination("MergeTree"), None);
    }

    #[test]
    fn soft_references() {
        let mut model = Model::default();
        let mut app = Database::new("app", "Atomic");
        app.tables
            .insert("events".into(), Table::new("events", "MergeTree"));
        app.views.insert("v".into(), View::default());
        let mut other = Database::new("other", "Atomic");
        other
            .tables
            .insert("users".into(), Table::new("users", "MergeTree"));
        model.databases.insert("app".into(), app);
        model.databases.insert("other".into(), other);
        model.functions.insert("f".into(), Function::default());
        let from = ObjectRef::new(ObjectKind::View, ["app", "v"]);
        let deps: Vec<String> = references(
            &model,
            "app",
            &from,
            "SELECT f(e.id), u.name FROM events AS e JOIN other.users AS u ON e.id = u.id \
             WHERE e.x IN (SELECT 1 FROM `app`.`v`)",
        )
        .map(|d| {
            assert!(d.soft);
            d.to.to_string()
        })
        .collect();
        assert_eq!(
            deps,
            ["table app.events", "table other.users", "function f"]
        );
    }
}
