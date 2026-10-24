//! References between objects that ordering needs: structural ones read from the model, and
//! guesses from identifiers in SQL text.

use crate::model::{Database, ForeignKey, KeyExpr, QualifiedName, Schema};
use openmig_core::sql::{Dialect, TokenKind, identifiers, tokenize};
use openmig_core::{ObjectKind, ObjectRef};
use std::collections::BTreeMap;

fn unquote_token(kind: TokenKind, text: &str) -> String {
    match kind {
        TokenKind::QuotedIdent => text[1..text.len().saturating_sub(1)].replace("\"\"", "\""),
        _ => text.to_ascii_lowercase(),
    }
}

/// `schema.name` pairs in SQL text (outside strings and comments).
pub(crate) fn qualified_names(sql: &str) -> Vec<(String, String)> {
    let Ok(tokens) = tokenize(sql, Dialect::Postgres) else {
        return Vec::new();
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .collect();
    let is_name = |k: TokenKind| matches!(k, TokenKind::Word | TokenKind::QuotedIdent);
    let mut out = Vec::new();
    for w in tokens.windows(3) {
        if is_name(w[0].kind)
            && w[1].kind == TokenKind::Punct
            && w[1].text == "."
            && is_name(w[2].kind)
        {
            out.push((
                unquote_token(w[0].kind, w[0].text),
                unquote_token(w[2].kind, w[2].text),
            ));
        }
    }
    out
}

/// Types defined in `db` that a type written as SQL text refers to (`app.status[]`,
/// `app.address`): enums, composite types, domains and relation row types.
pub(crate) fn type_refs(db: &Database, data_type: &str) -> Vec<ObjectRef> {
    let mut out = Vec::new();
    for (s, n) in qualified_names(data_type) {
        let Some(schema) = db.schemas.get(&s) else {
            continue;
        };
        let kind = if schema.enums.contains_key(&n) {
            ObjectKind::Enum
        } else if schema.composites.contains_key(&n) {
            ObjectKind::Composite
        } else if schema.domains.contains_key(&n) {
            ObjectKind::Domain
        } else if schema.tables.contains_key(&n) {
            ObjectKind::Table
        } else if schema.views.contains_key(&n) {
            ObjectKind::View
        } else if schema.materialized_views.contains_key(&n) {
            ObjectKind::MaterializedView
        } else {
            continue;
        };
        out.push(ObjectRef::new(kind, [s, n]));
    }
    out
}

/// Functions named `name` in `db` (every overload).
pub(crate) fn functions_named(db: &Database, name: &QualifiedName) -> Vec<ObjectRef> {
    let Some(schema) = db.schemas.get(&name.schema) else {
        return Vec::new();
    };
    schema
        .functions
        .iter()
        .filter(|(_, f)| f.name == name.name)
        .map(|(sig, _)| ObjectRef::new(ObjectKind::Function, [&name.schema, sig]))
        .collect()
}

/// The constraint (or unique index) of the referenced table that a foreign key relies on.
pub(crate) fn referenced_key(db: &Database, fk: &ForeignKey) -> Option<ObjectRef> {
    let t = db.table(&fk.ref_table)?;
    let (s, n) = (&fk.ref_table.schema, &fk.ref_table.name);
    let same = |cols: &[String]| {
        cols.len() == fk.ref_columns.len() && fk.ref_columns.iter().all(|c| cols.contains(c))
    };
    if let Some(pk) = &t.primary_key
        && same(&pk.columns)
    {
        return Some(ObjectRef::new(ObjectKind::PrimaryKey, [s, n, &pk.name]));
    }
    if let Some(u) = t.uniques.values().find(|u| same(&u.columns)) {
        return Some(ObjectRef::new(ObjectKind::Unique, [s, n, &u.name]));
    }
    t.standalone_indexes()
        .find(|i| {
            i.unique
                && i.predicate.is_none()
                && same(
                    &i.keys
                        .iter()
                        .filter_map(|k| match &k.expr {
                            KeyExpr::Column(c) => Some(c.clone()),
                            KeyExpr::Expression(_) => None,
                        })
                        .collect::<Vec<_>>(),
                )
        })
        .map(|i| ObjectRef::new(ObjectKind::Index, [s, n, &i.name]))
}

/// Schema-level objects by name, for matching identifiers in SQL text.
pub(crate) struct NameIndex {
    qualified: BTreeMap<(String, String), Vec<ObjectRef>>,
    unqualified: BTreeMap<String, Vec<ObjectRef>>,
}

impl NameIndex {
    /// Objects of `db`; unqualified names resolve in the schemas of `search_path`.
    pub fn new(db: &Database, search_path: &[String]) -> Self {
        let mut qualified: BTreeMap<(String, String), Vec<ObjectRef>> = BTreeMap::new();
        for schema in db.schemas.values() {
            for (name, r) in schema_objects(schema) {
                qualified
                    .entry((schema.name.clone(), name))
                    .or_default()
                    .push(r);
            }
        }
        let mut unqualified: BTreeMap<String, Vec<ObjectRef>> = BTreeMap::new();
        for s in search_path {
            if let Some(schema) = db.schemas.get(s) {
                for (name, r) in schema_objects(schema) {
                    unqualified.entry(name).or_default().push(r);
                }
            }
        }
        Self {
            qualified,
            unqualified,
        }
    }

    /// Objects that identifiers in `sql` may name.
    pub fn lookup(&self, sql: &str) -> Vec<ObjectRef> {
        let ids = identifiers(sql, Dialect::Postgres);
        let mut out = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            if let Some(next) = ids.get(i + 1)
                && let Some(found) = self.qualified.get(&(id.clone(), next.clone()))
            {
                out.extend(found.iter().cloned());
            }
            if let Some(found) = self.unqualified.get(id) {
                out.extend(found.iter().cloned());
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn schema_objects(s: &Schema) -> Vec<(String, ObjectRef)> {
    let n = &s.name;
    let mut out = Vec::new();
    let mut add = |kind: ObjectKind, name: &str, key: &str| {
        out.push((name.to_owned(), ObjectRef::new(kind, [n, key])));
    };
    for t in s.tables.keys() {
        add(ObjectKind::Table, t, t);
    }
    for v in s.views.keys() {
        add(ObjectKind::View, v, v);
    }
    for m in s.materialized_views.keys() {
        add(ObjectKind::MaterializedView, m, m);
    }
    for (sig, f) in &s.functions {
        add(ObjectKind::Function, &f.name, sig);
    }
    for (sig, f) in &s.procedures {
        add(ObjectKind::Procedure, &f.name, sig);
    }
    for q in s.sequences.keys() {
        add(ObjectKind::Sequence, q, q);
    }
    for e in s.enums.keys() {
        add(ObjectKind::Enum, e, e);
    }
    for c in s.composites.keys() {
        add(ObjectKind::Composite, c, c);
    }
    for d in s.domains.keys() {
        add(ObjectKind::Domain, d, d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Enum, Table};

    #[test]
    fn names_in_sql() {
        assert_eq!(
            qualified_names("app.status[] -- x.y\n, \"My S\".\"T\"(3), 'a.b'"),
            vec![
                ("app".to_owned(), "status".to_owned()),
                ("My S".to_owned(), "T".to_owned())
            ]
        );
        let mut db = Database::default();
        let mut s = Schema::new("app");
        s.enums.insert(
            "status".into(),
            Enum {
                name: "status".into(),
                ..Enum::default()
            },
        );
        s.tables.insert("users".into(), Table::new("users"));
        db.schemas.insert("app".into(), s);
        assert_eq!(
            type_refs(&db, "app.status[]"),
            vec![ObjectRef::new(ObjectKind::Enum, ["app", "status"])]
        );
        let index = NameIndex::new(&db, &["app".to_owned()]);
        assert_eq!(
            index.lookup("SELECT * FROM users u JOIN app.status s"),
            vec![
                ObjectRef::new(ObjectKind::Table, ["app", "users"]),
                ObjectRef::new(ObjectKind::Enum, ["app", "status"]),
            ]
        );
    }
}
