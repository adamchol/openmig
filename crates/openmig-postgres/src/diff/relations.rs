//! Views and materialized views.

use super::{AlterOp, Differ, merge};
use crate::model::{Schema, ViewColumn};
use openmig_core::{ObjectKind, ObjectRef};

/// Whether `CREATE OR REPLACE VIEW` can turn a view with `cur` columns into one with `des`
/// columns: the existing columns must stay, in order, with the same names and types.
pub(crate) fn columns_replaceable(cur: &[ViewColumn], des: &[ViewColumn]) -> bool {
    cur.len() <= des.len()
        && cur
            .iter()
            .zip(des)
            .all(|(c, d)| c.name == d.name && c.data_type == d.data_type)
}

impl Differ<'_> {
    pub(super) fn views(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.views, &des.views) {
            let r = ObjectRef::new(ObjectKind::View, [s, name]);
            let (c, d) = match (c, d) {
                (None, Some(_)) => {
                    self.create(r);
                    continue;
                }
                (Some(_), None) => {
                    self.drop(r);
                    continue;
                }
                (Some(c), Some(d)) => (c, d),
                (None, None) => continue,
            };
            let columns_same = c.columns.len() == d.columns.len()
                && c.columns
                    .iter()
                    .zip(&d.columns)
                    .all(|(a, b)| a.name == b.name && a.data_type == b.data_type);
            let mut ops = Vec::new();
            if c.query != d.query || !columns_same {
                if !columns_replaceable(&c.columns, &d.columns) {
                    self.recreate(r, "the columns of the view changed");
                    continue;
                }
                ops.push(AlterOp::Replace);
            } else if c.security_barrier != d.security_barrier
                || c.security_invoker != d.security_invoker
                || c.check_option != d.check_option
            {
                ops.push(AlterOp::ViewOptions);
            }
            Self::comment_op(&mut ops, &c.comment, &d.comment);
            self.acl_op(&mut ops, &c.acl, &d.acl);
            self.alter(r, ops);
            self.view_columns(s, name, &c.columns, &d.columns);
            self.triggers(s, name, &c.triggers, &d.triggers);
        }
    }

    /// Comments and privileges of view and materialized view columns.
    fn view_columns(&mut self, s: &str, rel: &str, cur: &[ViewColumn], des: &[ViewColumn]) {
        let empty = ViewColumn::default();
        for d in des {
            let c = cur.iter().find(|c| c.name == d.name).unwrap_or(&empty);
            let mut ops = Vec::new();
            Self::comment_op(&mut ops, &c.comment, &d.comment);
            self.acl_op(&mut ops, &c.acl, &d.acl);
            self.alter(ObjectRef::new(ObjectKind::Column, [s, rel, &d.name]), ops);
        }
    }

    pub(super) fn materialized_views(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.materialized_views, &des.materialized_views) {
            let r = ObjectRef::new(ObjectKind::MaterializedView, [s, name]);
            let (c, d) = match (c, d) {
                (None, Some(_)) => {
                    self.create(r);
                    continue;
                }
                (Some(_), None) => {
                    self.drop(r);
                    continue;
                }
                (Some(c), Some(d)) => (c, d),
                (None, None) => continue,
            };
            let columns_same = c.columns.len() == d.columns.len()
                && c.columns
                    .iter()
                    .zip(&d.columns)
                    .all(|(a, b)| a.name == b.name && a.data_type == b.data_type);
            if c.query != d.query || !columns_same {
                self.recreate(r, "the query changed");
                continue;
            }
            let mut ops = Vec::new();
            if c.storage_params != d.storage_params {
                ops.push(AlterOp::StorageParams);
            }
            if c.with_data != d.with_data {
                ops.push(AlterOp::Populate);
            }
            Self::comment_op(&mut ops, &c.comment, &d.comment);
            self.acl_op(&mut ops, &c.acl, &d.acl);
            self.alter(r, ops);
            self.view_columns(s, name, &c.columns, &d.columns);
            self.indexes(s, name, &c.indexes, &d.indexes);
        }
    }
}
