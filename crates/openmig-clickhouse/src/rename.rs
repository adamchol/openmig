//! Renames of tables and columns: from `renamed_from` hints, and candidates offered
//! interactively.

use crate::model::{Model, Table};
use openmig_core::{ObjectKind, ObjectRef, RenameCandidate, RenameHints, RenameSet};
use std::collections::BTreeSet;

fn table<'m>(model: &'m Model, database: &str, name: &str) -> Option<&'m Table> {
    model.databases.get(database)?.tables.get(name)
}

/// Hints whose old object exists in `current` and is gone from `desired`, and whose new object
/// is missing from `current`. Column hints may name the table by its new name when the table is
/// renamed too.
pub fn resolve_hints(current: &Model, desired: &Model, hints: &RenameHints) -> RenameSet {
    let mut set = RenameSet::default();
    for hint in hints.0.iter().filter(|h| h.kind == ObjectKind::Table) {
        let ([fd, fname], [td, tname]) = (hint.from.as_slice(), hint.to.as_slice()) else {
            continue;
        };
        let applies = table(current, fd, fname).is_some()
            && table(desired, fd, fname).is_none()
            && table(current, td, tname).is_none()
            && table(desired, td, tname).is_some();
        if applies {
            set.push(
                ObjectRef::new(ObjectKind::Table, [fd, fname]),
                ObjectRef::new(ObjectKind::Table, [td, tname]),
            );
        }
    }
    for hint in hints.0.iter().filter(|h| h.kind == ObjectKind::Column) {
        let ([_, _, old], [td, tname, new]) = (hint.from.as_slice(), hint.to.as_slice()) else {
            continue;
        };
        let source = set
            .of_kind(ObjectKind::Table)
            .find(|(_, to)| to.path == [td.clone(), tname.clone()])
            .map(|(from, _)| (from.path[0].clone(), from.path[1].clone()))
            .unwrap_or_else(|| (td.clone(), tname.clone()));
        let (Some(have), Some(want)) = (
            table(current, &source.0, &source.1),
            table(desired, td, tname),
        ) else {
            continue;
        };
        let applies = have.column(old).is_some()
            && have.column(new).is_none()
            && want.column(new).is_some()
            && want.column(old).is_none();
        if applies {
            set.push(
                ObjectRef::new(ObjectKind::Column, [&source.0, &source.1, old]),
                ObjectRef::new(ObjectKind::Column, [td, tname, new]),
            );
        }
    }
    set
}

/// Tables dropped and created in the same database whose columns overlap by at least 70%, and
/// columns dropped and added in the same table with the same type.
pub fn rename_candidates(current: &Model, desired: &Model) -> Vec<RenameCandidate> {
    let mut out = Vec::new();
    for (d, have_db) in &current.databases {
        let Some(want_db) = desired.databases.get(d) else {
            continue;
        };
        let dropped: Vec<&Table> = have_db
            .tables
            .values()
            .filter(|t| !want_db.tables.contains_key(&t.name))
            .collect();
        let created: Vec<&Table> = want_db
            .tables
            .values()
            .filter(|t| !have_db.tables.contains_key(&t.name))
            .collect();
        for old in &dropped {
            for new in &created {
                let a: BTreeSet<(&str, &str)> = old
                    .columns
                    .iter()
                    .map(|c| (c.name.as_str(), c.data_type.as_str()))
                    .collect();
                let b: BTreeSet<(&str, &str)> = new
                    .columns
                    .iter()
                    .map(|c| (c.name.as_str(), c.data_type.as_str()))
                    .collect();
                let union = a.union(&b).count();
                if union == 0 || old.engine != new.engine {
                    continue;
                }
                let score = a.intersection(&b).count() as f32 / union as f32;
                if score >= 0.7 {
                    out.push(RenameCandidate {
                        from: ObjectRef::new(ObjectKind::Table, [d, &old.name]),
                        to: ObjectRef::new(ObjectKind::Table, [d, &new.name]),
                        score,
                        reason: format!("{:.0}% of the columns match", score * 100.0),
                    });
                }
            }
        }
        for (name, have) in &have_db.tables {
            let Some(want) = want_db.tables.get(name) else {
                continue;
            };
            for old in have
                .columns
                .iter()
                .filter(|c| want.column(&c.name).is_none())
            {
                for new in want
                    .columns
                    .iter()
                    .filter(|c| have.column(&c.name).is_none())
                {
                    if old.data_type == new.data_type {
                        out.push(RenameCandidate {
                            from: ObjectRef::new(ObjectKind::Column, [d, name, &old.name]),
                            to: ObjectRef::new(ObjectKind::Column, [d, name, &new.name]),
                            score: 0.8,
                            reason: "same type".to_owned(),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Database};
    use openmig_core::RenameHint;

    fn model(table: &str, columns: &[&str]) -> Model {
        let mut t = Table::new(table, "MergeTree");
        t.columns = columns.iter().map(|c| Column::new(*c, "String")).collect();
        let mut db = Database::new("app", "Atomic");
        db.tables.insert(table.into(), t);
        let mut m = Model::default();
        m.databases.insert("app".into(), db);
        m
    }

    #[test]
    fn hints() {
        let current = model("hits", &["ts", "url"]);
        let desired = model("visits", &["ts", "page"]);
        let hints = RenameHints(vec![
            RenameHint {
                kind: ObjectKind::Table,
                from: vec!["app".into(), "hits".into()],
                to: vec!["app".into(), "visits".into()],
            },
            RenameHint {
                kind: ObjectKind::Column,
                from: vec!["app".into(), "visits".into(), "url".into()],
                to: vec!["app".into(), "visits".into(), "page".into()],
            },
        ]);
        let set = resolve_hints(&current, &desired, &hints);
        let pairs: Vec<String> = set.0.iter().map(|(a, b)| format!("{a} -> {b}")).collect();
        assert_eq!(
            pairs,
            [
                "table app.hits -> table app.visits",
                "column app.hits.url -> column app.visits.page"
            ]
        );
        assert!(resolve_hints(&desired, &desired, &hints).is_empty());

        let candidates =
            rename_candidates(&model("t", &["a", "b", "c"]), &model("t", &["a", "b", "d"]));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].to.dotted(), "app.t.d");
    }
}
