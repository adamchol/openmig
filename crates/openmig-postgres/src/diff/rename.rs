//! Table and column renames: resolving `renamed_from` hints, guessing candidates, and re-keying
//! the current model so that the diff sees renamed objects under their new names.

use crate::model::{ColumnRef, Database, Dependency, IndexKey, KeyExpr, QualifiedName, Table};
use openmig_core::sql::{Dialect, TokenKind, quote_ident, tokenize};
use openmig_core::{ObjectKind, ObjectRef, RenameCandidate, RenameHints, RenameSet};
use std::collections::BTreeSet;

/// Share of column names two tables must have in common to be offered as a rename.
const TABLE_OVERLAP: f32 = 0.7;

fn table_ref(name: &QualifiedName) -> ObjectRef {
    ObjectRef::new(ObjectKind::Table, [&name.schema, &name.name])
}

fn qname(r: &ObjectRef) -> Option<QualifiedName> {
    match r.path.as_slice() {
        [s, n] | [s, n, _] => Some(QualifiedName::new(s, n)),
        _ => None,
    }
}

/// Renames from `renamed_from` hints. A hint applies when the old object exists in `current`
/// and not in `desired`, and the new object exists in `desired` and not in `current`. Column
/// hints may name the table by its old or its new name.
pub fn resolve_hints(current: &Database, desired: &Database, hints: &RenameHints) -> RenameSet {
    let mut set = RenameSet::default();
    for h in hints.0.iter().filter(|h| h.kind == ObjectKind::Table) {
        let (Some(from), Some(to)) = (path2(&h.from), path2(&h.to)) else {
            continue;
        };
        if current.table(&from).is_some()
            && desired.table(&from).is_none()
            && desired.table(&to).is_some()
            && current.table(&to).is_none()
        {
            set.push(table_ref(&from), table_ref(&to));
        }
    }
    for h in hints.0.iter().filter(|h| h.kind == ObjectKind::Column) {
        let ([fs, ft, fc], [ts, tt, tc]) = (h.from.as_slice(), h.to.as_slice()) else {
            continue;
        };
        let to_table = QualifiedName::new(ts, tt);
        let mut from_table = QualifiedName::new(fs, ft);
        if current.table(&from_table).is_none() {
            let renamed = set
                .of_kind(ObjectKind::Table)
                .find(|(_, to)| qname(to).as_ref() == Some(&from_table))
                .and_then(|(from, _)| qname(from));
            match renamed {
                Some(t) => from_table = t,
                None => continue,
            }
        }
        let (Some(cur), Some(des)) = (current.table(&from_table), desired.table(&to_table)) else {
            continue;
        };
        let renamed_table = from_table != to_table;
        let old_in_desired = !renamed_table && des.column(fc).is_some();
        if cur.column(fc).is_some()
            && !old_in_desired
            && des.column(tc).is_some()
            && cur.column(tc).is_none()
        {
            set.push(
                ObjectRef::new(
                    ObjectKind::Column,
                    [&from_table.schema, &from_table.name, fc],
                ),
                ObjectRef::new(ObjectKind::Column, [ts, tt, tc]),
            );
        }
    }
    set
}

fn path2(p: &[String]) -> Option<QualifiedName> {
    match p {
        [s, n] => Some(QualifiedName::new(s, n)),
        _ => None,
    }
}

/// Drop + create pairs that look like renames: columns of the same table with the same type and
/// nullability, and tables of the same schema sharing at least 70% of their column names. Sorted
/// by score, best first.
pub fn rename_candidates(current: &Database, desired: &Database) -> Vec<RenameCandidate> {
    let mut out = Vec::new();
    for (sname, cs) in &current.schemas {
        let Some(ds) = desired.schemas.get(sname) else {
            continue;
        };
        for (tname, ct) in &cs.tables {
            let Some(dt) = ds.tables.get(tname) else {
                continue;
            };
            let dropped: Vec<_> = ct
                .columns
                .iter()
                .filter(|c| dt.column(&c.name).is_none())
                .collect();
            let added: Vec<_> = dt
                .columns
                .iter()
                .filter(|c| ct.column(&c.name).is_none())
                .collect();
            for old in &dropped {
                for new in &added {
                    if old.data_type != new.data_type || old.not_null != new.not_null {
                        continue;
                    }
                    let same_default = old.default == new.default;
                    out.push(RenameCandidate {
                        from: ObjectRef::new(ObjectKind::Column, [sname, tname, &old.name]),
                        to: ObjectRef::new(ObjectKind::Column, [sname, tname, &new.name]),
                        score: if same_default { 0.9 } else { 0.8 },
                        reason: "same type and nullability".into(),
                    });
                }
            }
        }
        let dropped: Vec<&Table> = cs
            .tables
            .values()
            .filter(|t| !ds.tables.contains_key(&t.name))
            .collect();
        let added: Vec<&Table> = ds
            .tables
            .values()
            .filter(|t| !cs.tables.contains_key(&t.name))
            .collect();
        for old in &dropped {
            for new in &added {
                let a: BTreeSet<&str> = old.columns.iter().map(|c| c.name.as_str()).collect();
                let b: BTreeSet<&str> = new.columns.iter().map(|c| c.name.as_str()).collect();
                let total = a.len().max(b.len());
                if total == 0 {
                    continue;
                }
                let common = a.intersection(&b).count();
                #[allow(clippy::cast_precision_loss)]
                let overlap = common as f32 / total as f32;
                if overlap >= TABLE_OVERLAP {
                    out.push(RenameCandidate {
                        from: ObjectRef::new(ObjectKind::Table, [sname, &old.name]),
                        to: ObjectRef::new(ObjectKind::Table, [sname, &new.name]),
                        score: overlap,
                        reason: format!("{}% of the columns in common", (overlap * 100.0).round()),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
    });
    out
}

/// The renames of `set` that can be applied to `current`, tables first, with column renames
/// expressed against the renamed table.
pub(crate) fn applicable(current: &Database, set: &RenameSet) -> Vec<(ObjectRef, ObjectRef)> {
    let mut out = Vec::new();
    let mut tables: Vec<(QualifiedName, QualifiedName)> = Vec::new();
    for (from, to) in set.of_kind(ObjectKind::Table) {
        let (Some(f), Some(t)) = (qname(from), qname(to)) else {
            continue;
        };
        if to.kind == ObjectKind::Table
            && current.table(&f).is_some()
            && current.table(&t).is_none()
            && !tables.iter().any(|(a, b)| a == &f || b == &t)
        {
            tables.push((f, t));
            out.push((from.clone(), to.clone()));
        }
    }
    for (from, to) in set.of_kind(ObjectKind::Column) {
        let ([fs, ft, fc], [ts, tt, tc]) = (from.path.as_slice(), to.path.as_slice()) else {
            continue;
        };
        let f = QualifiedName::new(fs, ft);
        let target = tables
            .iter()
            .find(|(a, _)| a == &f)
            .map_or_else(|| f.clone(), |(_, b)| b.clone());
        if target != QualifiedName::new(ts, tt) || to.kind != ObjectKind::Column {
            continue;
        }
        let Some(t) = current.table(&f) else {
            continue;
        };
        if t.column(fc).is_some() && t.column(tc).is_none() {
            out.push((from.clone(), to.clone()));
        }
    }
    out
}

/// Schemas that tables are moved into but that do not exist in `current`. [`rekey`] adds them
/// (empty) so that the moved tables have a place; the diff still creates them.
pub(crate) fn new_schemas(
    current: &Database,
    renames: &[(ObjectRef, ObjectRef)],
) -> BTreeSet<String> {
    renames
        .iter()
        .filter(|(from, _)| from.kind == ObjectKind::Table)
        .map(|(_, to)| to.path[0].clone())
        .filter(|s| !current.schemas.contains_key(s))
        .collect()
}

/// `current` with the renames applied, as PostgreSQL would show it after running them.
pub(crate) fn rekey(current: &Database, renames: &[(ObjectRef, ObjectRef)]) -> Database {
    let mut db = current.clone();
    for s in new_schemas(current, renames) {
        db.schemas.insert(s.clone(), crate::model::Schema::new(s));
    }
    for (from, to) in renames {
        match from.kind {
            ObjectKind::Table => {
                if let (Some(f), Some(t)) = (qname(from), qname(to)) {
                    rename_table(&mut db, &f, &t);
                }
            }
            ObjectKind::Column => {
                if let ([fs, ft, fc], [ts, tt, tc]) = (from.path.as_slice(), to.path.as_slice()) {
                    let renamed = renames
                        .iter()
                        .find(|(a, _)| {
                            a.kind == ObjectKind::Table && a.path == [fs.clone(), ft.clone()]
                        })
                        .is_some();
                    let table = if renamed {
                        QualifiedName::new(ts, tt)
                    } else {
                        QualifiedName::new(fs, ft)
                    };
                    rename_column(&mut db, &table, fc, tc);
                }
            }
            _ => {}
        }
    }
    db.normalize_dependencies();
    db
}

fn rewrite_refs(db: &mut Database, f: impl Fn(&ObjectRef) -> Option<ObjectRef>) {
    let deps = std::mem::take(&mut db.dependencies);
    db.dependencies = deps
        .into_iter()
        .map(|d| Dependency {
            from: f(&d.from).unwrap_or(d.from),
            to: f(&d.to).unwrap_or(d.to),
        })
        .collect();
}

fn rename_table(db: &mut Database, from: &QualifiedName, to: &QualifiedName) {
    let Some(mut table) = db
        .schemas
        .get_mut(&from.schema)
        .and_then(|s| s.tables.remove(&from.name))
    else {
        return;
    };
    table.name.clone_from(&to.name);
    let moved_sequences: Vec<String> = if from.schema == to.schema {
        Vec::new()
    } else {
        db.schemas[&from.schema]
            .sequences
            .values()
            .filter(|s| {
                s.owned_by
                    .as_ref()
                    .is_some_and(|o| o.schema == from.schema && o.table == from.name)
            })
            .map(|s| s.name.clone())
            .collect()
    };
    if let Some(schema) = db.schemas.get_mut(&to.schema) {
        schema.tables.insert(to.name.clone(), table);
    }
    for name in &moved_sequences {
        let seq = db
            .schemas
            .get_mut(&from.schema)
            .and_then(|s| s.sequences.remove(name));
        if let (Some(seq), Some(schema)) = (seq, db.schemas.get_mut(&to.schema)) {
            schema.sequences.insert(name.clone(), seq);
        }
    }
    for schema in db.schemas.values_mut() {
        for t in schema.tables.values_mut() {
            for fk in t.foreign_keys.values_mut() {
                if &fk.ref_table == from {
                    fk.ref_table = to.clone();
                }
            }
            if let Some(p) = &mut t.partition_of
                && &p.parent == from
            {
                p.parent = to.clone();
            }
            for parent in &mut t.inherits {
                if parent == from {
                    *parent = to.clone();
                }
            }
        }
        for s in schema.sequences.values_mut() {
            if let Some(o) = &mut s.owned_by
                && o.schema == from.schema
                && o.table == from.name
            {
                o.schema.clone_from(&to.schema);
                o.table.clone_from(&to.name);
            }
        }
    }
    if let Some(data) = db.data.remove(from) {
        db.data.insert(to.clone(), data);
    }
    for name in &moved_sequences {
        // Defaults name sequences by OID; PostgreSQL prints the moved sequence's new schema.
        let printed = |schema: &str| {
            format!(
                "'{}.{}'::regclass",
                printed_ident(schema),
                printed_ident(name)
            )
        };
        let (old_text, new_text) = (printed(&from.schema), printed(&to.schema));
        for schema in db.schemas.values_mut() {
            for t in schema.tables.values_mut() {
                for c in &mut t.columns {
                    if let Some(d) = &mut c.default
                        && d.contains(&old_text)
                    {
                        *d = d.replace(&old_text, &new_text);
                    }
                }
            }
        }
    }
    let old = [from.schema.clone(), from.name.clone()];
    rewrite_refs(db, |r| {
        if r.kind == ObjectKind::Sequence
            && r.path.len() == 2
            && r.path[0] == from.schema
            && moved_sequences.contains(&r.path[1])
        {
            return Some(ObjectRef::new(
                ObjectKind::Sequence,
                [&to.schema, &r.path[1]],
            ));
        }
        let affected = match r.path.len() {
            2 => matches!(r.kind, ObjectKind::Table | ObjectKind::Data) && r.path[..] == old,
            3 => r.path[..2] == old,
            _ => false,
        };
        if affected {
            let mut path = vec![to.schema.clone(), to.name.clone()];
            path.extend(r.path[2..].iter().cloned());
            return Some(ObjectRef { kind: r.kind, path });
        }
        None
    });
}

fn rename_column(db: &mut Database, table: &QualifiedName, from: &str, to: &str) {
    let rename = |c: &mut String| {
        if c == from {
            *c = to.to_owned();
        }
    };
    let rename_key = |k: &mut IndexKey| match &mut k.expr {
        KeyExpr::Column(c) => {
            if c == from {
                *c = to.to_owned();
            }
        }
        KeyExpr::Expression(e) => *e = rename_identifier(e, from, to),
    };
    let Some(t) = db
        .schemas
        .get_mut(&table.schema)
        .and_then(|s| s.tables.get_mut(&table.name))
    else {
        return;
    };
    let Some(column) = t.column_mut(from) else {
        return;
    };
    column.name = to.to_owned();
    for c in &mut t.columns {
        if let Some(g) = &mut c.generated {
            g.expr = rename_identifier(&g.expr, from, to);
        }
    }
    if let Some(pk) = &mut t.primary_key {
        pk.columns.iter_mut().for_each(rename);
        pk.include.iter_mut().for_each(rename);
    }
    for u in t.uniques.values_mut() {
        u.columns.iter_mut().for_each(rename);
        u.include.iter_mut().for_each(rename);
    }
    for fk in t.foreign_keys.values_mut() {
        fk.columns.iter_mut().for_each(rename);
    }
    for c in t.checks.values_mut() {
        c.expr = rename_identifier(&c.expr, from, to);
    }
    for x in t.exclusions.values_mut() {
        x.elements.iter_mut().for_each(|e| rename_key(&mut e.key));
        x.include.iter_mut().for_each(rename);
        if let Some(p) = &mut x.predicate {
            *p = rename_identifier(p, from, to);
        }
    }
    for i in t.indexes.values_mut() {
        i.keys.iter_mut().for_each(rename_key);
        i.include.iter_mut().for_each(rename);
        if let Some(p) = &mut i.predicate {
            *p = rename_identifier(p, from, to);
        }
    }
    for p in t.policies.values_mut() {
        if let Some(u) = &mut p.using {
            *u = rename_identifier(u, from, to);
        }
        if let Some(c) = &mut p.check {
            *c = rename_identifier(c, from, to);
        }
    }
    for g in t.triggers.values_mut() {
        g.update_of.iter_mut().for_each(rename);
    }
    for schema in db.schemas.values_mut() {
        for other in schema.tables.values_mut() {
            for fk in other.foreign_keys.values_mut() {
                if &fk.ref_table == table {
                    fk.ref_columns.iter_mut().for_each(rename);
                }
            }
        }
        for s in schema.sequences.values_mut() {
            if let Some(ColumnRef {
                schema,
                table: t,
                column,
            }) = &mut s.owned_by
                && *schema == table.schema
                && *t == table.name
                && column == from
            {
                *column = to.to_owned();
            }
        }
    }
    if let Some(data) = db.data.get_mut(table) {
        data.key.iter_mut().for_each(rename);
        for row in data.rows.values_mut() {
            if let Some(v) = row.remove(from) {
                row.insert(to.to_owned(), v);
            }
        }
    }
    let old = ObjectRef::new(ObjectKind::Column, [&table.schema, &table.name, from]);
    let new = ObjectRef::new(ObjectKind::Column, [&table.schema, &table.name, to]);
    rewrite_refs(db, |r| (r == &old).then(|| new.clone()));
}

/// `expr` with unqualified identifier tokens naming `from` replaced by `to`, quoted the way
/// PostgreSQL prints it. Used so that expressions of a table whose column is renamed compare equal to what
/// PostgreSQL shows after the rename.
fn rename_identifier(expr: &str, from: &str, to: &str) -> String {
    let Ok(tokens) = tokenize(expr, Dialect::Postgres) else {
        return expr.to_owned();
    };
    let mut out = String::with_capacity(expr.len());
    let mut after_dot = false;
    for t in tokens {
        let matches = !after_dot
            && match t.kind {
                TokenKind::Word => t.text.to_ascii_lowercase() == from,
                TokenKind::QuotedIdent => t.text == quote_ident(from, Dialect::Postgres),
                _ => false,
            };
        after_dot = t.kind == TokenKind::Punct && t.text == ".";
        if matches {
            out.push_str(&printed_ident(to));
        } else {
            out.push_str(t.text);
        }
    }
    out
}

/// An identifier as PostgreSQL prints it: bare when it is a plain lower-case word.
fn printed_ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_owned()
    } else {
        quote_ident(name, Dialect::Postgres)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Check, Column, ForeignKey, PrimaryKey, Schema};
    use openmig_core::RenameHint;

    fn db(tables: &[(&str, &str, &[&str])]) -> Database {
        let mut db = Database::default();
        for (s, t, cols) in tables {
            let schema = db
                .schemas
                .entry((*s).to_owned())
                .or_insert_with(|| Schema::new(*s));
            let mut table = Table::new(*t);
            for c in *cols {
                table.columns.push(Column::new(*c, "text"));
            }
            schema.tables.insert((*t).to_owned(), table);
        }
        db
    }

    #[test]
    fn hints() {
        let current = db(&[("app", "people", &["id", "name"])]);
        let desired = db(&[("app", "users", &["id", "full_name"])]);
        let hints = RenameHints(vec![
            RenameHint {
                kind: ObjectKind::Table,
                from: vec!["app".into(), "people".into()],
                to: vec!["app".into(), "users".into()],
            },
            RenameHint {
                kind: ObjectKind::Column,
                from: vec!["app".into(), "users".into(), "name".into()],
                to: vec!["app".into(), "users".into(), "full_name".into()],
            },
            RenameHint {
                kind: ObjectKind::Column,
                from: vec!["app".into(), "users".into(), "gone".into()],
                to: vec!["app".into(), "users".into(), "id".into()],
            },
        ]);
        let set = resolve_hints(&current, &desired, &hints);
        assert_eq!(
            set.0,
            vec![
                (
                    ObjectRef::new(ObjectKind::Table, ["app", "people"]),
                    ObjectRef::new(ObjectKind::Table, ["app", "users"])
                ),
                (
                    ObjectRef::new(ObjectKind::Column, ["app", "people", "name"]),
                    ObjectRef::new(ObjectKind::Column, ["app", "users", "full_name"])
                ),
            ]
        );
        let applied = applicable(&current, &set);
        assert_eq!(applied.len(), 2);
        let rekeyed = rekey(&current, &applied);
        let t = rekeyed.table(&QualifiedName::new("app", "users")).unwrap();
        assert_eq!(
            t.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "full_name"]
        );
        // Applied once, the hint no longer matches.
        assert!(resolve_hints(&rekeyed, &desired, &hints).is_empty());
    }

    #[test]
    fn rekey_updates_references() {
        let mut current = db(&[
            ("app", "orgs", &["id", "name"]),
            ("app", "users", &["id", "org"]),
        ]);
        current.schemas.insert("other".into(), Schema::new("other"));
        {
            let users = current
                .schemas
                .get_mut("app")
                .unwrap()
                .tables
                .get_mut("users")
                .unwrap();
            users.foreign_keys.insert(
                "fk".into(),
                ForeignKey {
                    name: "fk".into(),
                    columns: vec!["org".into()],
                    ref_table: QualifiedName::new("app", "orgs"),
                    ref_columns: vec!["id".into()],
                    on_update: Default::default(),
                    on_delete: Default::default(),
                    match_type: Default::default(),
                    deferrable: false,
                    initially_deferred: false,
                    validated: true,
                    enforced: true,
                    comment: None,
                },
            );
            users.checks.insert(
                "c".into(),
                Check {
                    name: "c".into(),
                    expr: "(org IS NOT NULL)".into(),
                    no_inherit: false,
                    validated: true,
                    enforced: true,
                    comment: None,
                },
            );
            users.primary_key = Some(PrimaryKey {
                name: "users_pkey".into(),
                columns: vec!["org".into()],
                ..PrimaryKey::default()
            });
        }
        current.dependencies.push(Dependency {
            from: ObjectRef::new(ObjectKind::ForeignKey, ["app", "users", "fk"]),
            to: ObjectRef::new(ObjectKind::Column, ["app", "orgs", "id"]),
        });
        let renames = vec![
            (
                ObjectRef::new(ObjectKind::Table, ["app", "orgs"]),
                ObjectRef::new(ObjectKind::Table, ["other", "orgs2"]),
            ),
            (
                ObjectRef::new(ObjectKind::Column, ["app", "users", "org"]),
                ObjectRef::new(ObjectKind::Column, ["app", "users", "Org Id"]),
            ),
        ];
        let db = rekey(&current, &renames);
        let users = db.table(&QualifiedName::new("app", "users")).unwrap();
        let fk = &users.foreign_keys["fk"];
        assert_eq!(fk.ref_table, QualifiedName::new("other", "orgs2"));
        assert_eq!(fk.columns, ["Org Id"]);
        assert_eq!(users.checks["c"].expr, "(\"Org Id\" IS NOT NULL)");
        assert_eq!(users.primary_key.as_ref().unwrap().columns, ["Org Id"]);
        assert_eq!(
            db.dependencies[0].to,
            ObjectRef::new(ObjectKind::Column, ["other", "orgs2", "id"])
        );
    }

    #[test]
    fn candidates() {
        let current = db(&[
            ("app", "people", &["id", "name", "email", "phone"]),
            ("app", "t", &["a", "old"]),
        ]);
        let desired = db(&[
            ("app", "users", &["id", "name", "email", "phone", "x"]),
            ("app", "t", &["a", "new"]),
        ]);
        let c = rename_candidates(&current, &desired);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].from.dotted(), "app.t.old");
        assert_eq!(c[0].to.dotted(), "app.t.new");
        assert_eq!(c[1].from.dotted(), "app.people");
        assert!((c[1].score - 0.8).abs() < 1e-6);
    }

    #[test]
    fn identifiers_in_expressions() {
        assert_eq!(
            rename_identifier("(a > 0) AND (\"a\" < b) AND 'a' <> x.a", "a", "c"),
            "(c > 0) AND (c < b) AND 'a' <> x.a"
        );
        assert_eq!(rename_identifier("lower(a)", "a", "B"), "lower(\"B\")");
    }
}
