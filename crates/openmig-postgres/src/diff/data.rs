//! Rows of data-managed tables.

use super::{Change, DataChanges, Differ, Row};
use crate::model::{QualifiedName, TableData};
use openmig_core::DataSync;
use std::collections::BTreeMap;

impl Differ<'_> {
    /// Rows are compared by the desired key, on the columns the `data` block declares. Tables
    /// without declared rows, or outside [`openmig_core::Scope::data_tables`], are not touched.
    pub(super) fn data(&mut self) {
        let empty = TableData::default();
        for managed in &self.opts.scope.data_tables {
            let [s, t] = managed.path.as_slice() else {
                continue;
            };
            let name = QualifiedName::new(s, t);
            let Some(des) = self.des.data.get(&name) else {
                continue;
            };
            if self.des.table(&name).is_none() {
                continue;
            }
            let mode = managed.mode.or(des.mode).unwrap_or(DataSync::Sync);
            let cur = self.cur.data.get(&name).unwrap_or(&empty);
            let key = des.key.clone();
            let compared = |c: &String| managed.columns.is_empty() || managed.columns.contains(c);
            let cur_rows: BTreeMap<Vec<Option<String>>, &Row> = cur
                .rows
                .values()
                .map(|r| {
                    let k = key.iter().map(|c| r.get(c).cloned().flatten()).collect();
                    (k, r)
                })
                .collect();
            let mut rows = DataChanges {
                key: key.clone(),
                ..DataChanges::default()
            };
            for (k, d) in &des.rows {
                let d: Row = d
                    .iter()
                    .filter(|(c, _)| compared(c) || key.contains(c))
                    .map(|(c, v)| (c.clone(), v.clone()))
                    .collect();
                match cur_rows.get(k) {
                    None => rows.insert.push(d),
                    Some(c) if mode == DataSync::Sync => {
                        let set: Row = d
                            .iter()
                            .filter(|(col, v)| !key.contains(col) && c.get(*col) != Some(v))
                            .map(|(col, v)| (col.clone(), v.clone()))
                            .collect();
                        if !set.is_empty() {
                            rows.update.push((k.clone(), set));
                        }
                    }
                    Some(_) => {}
                }
            }
            if mode == DataSync::Sync {
                rows.delete = cur_rows
                    .keys()
                    .filter(|k| !des.rows.contains_key(*k))
                    .cloned()
                    .collect();
            }
            if !rows.is_empty() {
                self.push(Change::Data { table: name, rows });
            }
        }
    }
}
