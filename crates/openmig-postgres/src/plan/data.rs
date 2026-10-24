//! Steps for rows of data-managed tables.

use super::Planner;
use super::step::{Phase, Step, StepKind, rank};
use crate::diff::DataChanges;
use crate::model::{IdentityGeneration, QualifiedName};
use crate::sql;
use openmig_core::{ObjectKind, ObjectRef};
use std::collections::BTreeMap;

/// Rows per `INSERT` statement.
const INSERT_BATCH: usize = 100;

impl Planner<'_> {
    /// A delete step and an update-and-insert step. Ordering between tables follows their
    /// foreign keys (see `order`).
    pub(super) fn data(&mut self, table: &QualifiedName, rows: &DataChanges) {
        let r = ObjectRef::new(ObjectKind::Data, [&table.schema, &table.name]);
        let table_ref = ObjectRef::new(ObjectKind::Table, [&table.schema, &table.name]);
        let key = |values: &[Option<String>]| -> Vec<(String, Option<String>)> {
            rows.key
                .iter()
                .cloned()
                .zip(values.iter().cloned())
                .collect()
        };
        if !rows.delete.is_empty() {
            let mut step = Step::new(&r, Phase::Create, rank::DATA_DELETE, " delete");
            step.kind = StepKind::Data;
            for k in &rows.delete {
                let k = key(k);
                let k: Vec<(&str, Option<&str>)> =
                    k.iter().map(|(c, v)| (c.as_str(), v.as_deref())).collect();
                let st = step.sql(
                    sql::delete_row(table, &k),
                    format!("Delete a row from {}", table.to_sql()),
                );
                st.destructive = Some(format!("deletes a row from {table}"));
            }
            step.requires.push(table_ref.clone());
            self.push(step);
        }
        if rows.insert.is_empty() && rows.update.is_empty() {
            return;
        }
        let mut step = Step::new(&r, Phase::Create, rank::DATA_UPSERT, " upsert");
        step.kind = StepKind::Data;
        step.requires.push(table_ref);
        for (k, set) in &rows.update {
            let k = key(k);
            let k: Vec<(&str, Option<&str>)> =
                k.iter().map(|(c, v)| (c.as_str(), v.as_deref())).collect();
            let set: Vec<(&str, Option<&str>)> = set
                .iter()
                .map(|(c, v)| (c.as_str(), v.as_deref()))
                .collect();
            step.sql(
                sql::update_row(table, &set, &k),
                format!("Update a row of {}", table.to_sql()),
            );
            for (c, _) in &set {
                step.requires.push(ObjectRef::new(
                    ObjectKind::Column,
                    [&table.schema, &table.name, *c],
                ));
            }
        }
        let always: Vec<&str> = self
            .des
            .table(table)
            .map(|t| {
                t.columns
                    .iter()
                    .filter(|c| {
                        c.identity
                            .as_ref()
                            .is_some_and(|i| i.generation == IdentityGeneration::Always)
                    })
                    .map(|c| c.name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let mut by_columns: BTreeMap<Vec<String>, Vec<Vec<Option<&str>>>> = BTreeMap::new();
        for row in &rows.insert {
            let columns: Vec<String> = row.keys().cloned().collect();
            let values = row.values().map(Option::as_deref).collect();
            by_columns.entry(columns).or_default().push(values);
        }
        for (columns, values) in &by_columns {
            let overriding = columns.iter().any(|c| always.contains(&c.as_str()));
            for chunk in values.chunks(INSERT_BATCH) {
                step.sql(
                    sql::insert_rows(table, columns, chunk, overriding),
                    format!("Insert rows into {}", table.to_sql()),
                );
            }
            for c in columns {
                step.requires.push(ObjectRef::new(
                    ObjectKind::Column,
                    [&table.schema, &table.name, c],
                ));
            }
        }
        self.push(step);
    }
}
