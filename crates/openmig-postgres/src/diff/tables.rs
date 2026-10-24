//! Tables and everything nested in them.

use super::{AlterOp, Differ, merge};
use crate::model::{Column, Database, GeneratedKind, Policy, Schema, Table, Trigger};
use openmig_core::{ObjectKind, ObjectRef};
use std::collections::{BTreeMap, BTreeSet};

/// Whether `a` and `b` are equal once `strip` has cleared the fields that are compared
/// separately (comments, validation state, ...).
pub(super) fn same_except<T: Clone + PartialEq>(a: &T, b: &T, strip: impl Fn(&mut T)) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    strip(&mut a);
    strip(&mut b);
    a == b
}

/// Columns of `t` that are inherited and therefore managed through the parent: all of them for
/// a partition, the parents' columns for an `INHERITS` child.
pub(crate) fn inherited_columns(db: &Database, t: &Table) -> BTreeSet<String> {
    if t.partition_of.is_some() {
        return t.columns.iter().map(|c| c.name.clone()).collect();
    }
    t.inherits
        .iter()
        .filter_map(|p| db.table(p))
        .flat_map(|p| p.columns.iter().map(|c| c.name.clone()))
        .collect()
}

impl Differ<'_> {
    pub(super) fn tables(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.tables, &des.tables) {
            let r = ObjectRef::new(ObjectKind::Table, [s, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => self.table(s, c, d),
                (None, None) => {}
            }
        }
    }

    fn table(&mut self, s: &str, cur: &Table, des: &Table) {
        let r = ObjectRef::new(ObjectKind::Table, [s, &des.name]);
        if cur.partition_by != des.partition_by {
            self.unsupported(r, "the partitioning of the table changed");
            return;
        }
        let mut ops = Vec::new();
        if cur.partition_of != des.partition_of {
            ops.push(AlterOp::Partition);
        }
        let cur_parents: BTreeSet<_> = cur.inherits.iter().collect();
        let des_parents: BTreeSet<_> = des.inherits.iter().collect();
        for p in des_parents.difference(&cur_parents) {
            ops.push(AlterOp::Inherit((*p).clone()));
        }
        for p in cur_parents.difference(&des_parents) {
            ops.push(AlterOp::NoInherit((*p).clone()));
        }
        if cur.unlogged != des.unlogged {
            ops.push(AlterOp::Persistence);
        }
        if cur.storage_params != des.storage_params {
            ops.push(AlterOp::StorageParams);
        }
        if cur.rls_enabled != des.rls_enabled {
            ops.push(AlterOp::RowSecurity);
        }
        if cur.rls_forced != des.rls_forced {
            ops.push(AlterOp::ForceRowSecurity);
        }
        if cur.replica_identity != des.replica_identity {
            ops.push(AlterOp::ReplicaIdentity);
        }
        Self::comment_op(&mut ops, &cur.comment, &des.comment);
        self.acl_op(&mut ops, &cur.acl, &des.acl);
        self.alter(r, ops);

        self.columns(s, cur, des);
        self.constraints(s, cur, des);
        self.indexes(s, &des.name, &cur.indexes, &des.indexes);
        self.policies(s, cur, des);
        self.triggers(s, &des.name, &cur.triggers, &des.triggers);
    }

    fn columns(&mut self, s: &str, cur: &Table, des: &Table) {
        let skip = inherited_columns(self.des, des);
        let t = &des.name;
        let cur_cols: BTreeMap<&str, &Column> =
            cur.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        let des_cols: BTreeMap<&str, &Column> =
            des.columns.iter().map(|c| (c.name.as_str(), c)).collect();
        for (name, c, d) in merge(&cur_cols, &des_cols) {
            if skip.contains(*name) {
                continue;
            }
            let r = ObjectRef::new(ObjectKind::Column, [s, t, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => self.column(r, c, d),
                (None, None) => {}
            }
        }
    }

    fn column(&mut self, r: ObjectRef, cur: &Column, des: &Column) {
        let mut ops = Vec::new();
        match (&cur.generated, &des.generated) {
            (None, None) => {}
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b))
                if a.kind == GeneratedKind::Stored && b.kind == GeneratedKind::Stored =>
            {
                if self.opts.major < 17 {
                    self.unsupported(
                        r,
                        "changing the expression of a generated column needs PostgreSQL 17 or later",
                    );
                    return;
                }
                ops.push(AlterOp::Generated);
            }
            (Some(_), Some(_)) => {
                // Generated values are recomputed, so nothing is lost; `SET EXPRESSION` is
                // refused for virtual columns of tables with checks.
                self.recreate(r, "the kind or expression of a generated column changed");
                return;
            }
            (Some(a), None) if a.kind == GeneratedKind::Stored => ops.push(AlterOp::Generated),
            (Some(_), None) => {
                self.unsupported(r, "a virtual generated column cannot become a plain column");
                return;
            }
            (None, Some(_)) => {
                self.unsupported(r, "an existing column cannot become a generated column");
                return;
            }
        }
        if cur.identity != des.identity {
            ops.push(AlterOp::Identity);
        }
        if cur.data_type != des.data_type || cur.collation != des.collation {
            ops.push(AlterOp::Type);
        }
        if cur.not_null != des.not_null {
            ops.push(AlterOp::NotNull);
        }
        if cur.default != des.default {
            ops.push(AlterOp::Default);
        }
        if cur.storage != des.storage {
            ops.push(AlterOp::Storage);
        }
        if cur.compression != des.compression {
            ops.push(AlterOp::Compression);
        }
        Self::comment_op(&mut ops, &cur.comment, &des.comment);
        self.acl_op(&mut ops, &cur.acl, &des.acl);
        self.alter(r, ops);
    }

    fn constraints(&mut self, s: &str, cur: &Table, des: &Table) {
        let t = &des.name;
        let pk = |name: &str| ObjectRef::new(ObjectKind::PrimaryKey, [s, t, name]);
        match (&cur.primary_key, &des.primary_key) {
            (None, Some(d)) => self.create(pk(&d.name)),
            (Some(c), None) => self.drop(pk(&c.name)),
            (Some(c), Some(d)) if c.name != d.name => {
                self.drop(pk(&c.name));
                self.create(pk(&d.name));
            }
            (Some(c), Some(d)) => {
                if same_except(c, d, |x| x.comment = None) {
                    let mut ops = Vec::new();
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.alter(pk(&d.name), ops);
                } else {
                    self.recreate(pk(&d.name), "primary key changed");
                }
            }
            (None, None) => {}
        }
        for (name, c, d) in merge(&cur.foreign_keys, &des.foreign_keys) {
            let r = ObjectRef::new(ObjectKind::ForeignKey, [s, t, name]);
            self.constraint(
                r,
                c,
                d,
                |x| {
                    x.comment = None;
                    x.validated = true;
                },
                |x| (x.validated, &x.comment),
            );
        }
        for (name, c, d) in merge(&cur.checks, &des.checks) {
            let r = ObjectRef::new(ObjectKind::Check, [s, t, name]);
            self.constraint(
                r,
                c,
                d,
                |x| {
                    x.comment = None;
                    x.validated = true;
                },
                |x| (x.validated, &x.comment),
            );
        }
        for (name, c, d) in merge(&cur.uniques, &des.uniques) {
            let r = ObjectRef::new(ObjectKind::Unique, [s, t, name]);
            self.constraint(r, c, d, |x| x.comment = None, |x| (true, &x.comment));
        }
        for (name, c, d) in merge(&cur.exclusions, &des.exclusions) {
            let r = ObjectRef::new(ObjectKind::Exclusion, [s, t, name]);
            self.constraint(r, c, d, |x| x.comment = None, |x| (true, &x.comment));
        }
    }

    /// A constraint is recreated when its definition changes; `extra` gives its validation state
    /// and comment, compared separately.
    pub(super) fn constraint<T: Clone + PartialEq>(
        &mut self,
        r: ObjectRef,
        cur: Option<&T>,
        des: Option<&T>,
        strip: impl Fn(&mut T),
        extra: impl Fn(&T) -> (bool, &Option<String>),
    ) {
        match (cur, des) {
            (None, Some(_)) => self.create(r),
            (Some(_), None) => self.drop(r),
            (Some(c), Some(d)) => {
                let (c_valid, c_comment) = extra(c);
                let (d_valid, d_comment) = extra(d);
                if !same_except(c, d, strip) || (c_valid && !d_valid) {
                    self.recreate(r, "definition changed");
                    return;
                }
                let mut ops = Vec::new();
                if !c_valid && d_valid {
                    ops.push(AlterOp::Validate);
                }
                Self::comment_op(&mut ops, c_comment, d_comment);
                self.alter(r, ops);
            }
            (None, None) => {}
        }
    }

    /// Indexes that do not implement a constraint, of a table or materialized view.
    pub(super) fn indexes(
        &mut self,
        s: &str,
        rel: &str,
        cur: &BTreeMap<String, crate::model::Index>,
        des: &BTreeMap<String, crate::model::Index>,
    ) {
        let standalone = |m: &BTreeMap<String, crate::model::Index>| {
            m.iter()
                .filter(|(_, i)| !i.constraint)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let (cur, des) = (standalone(cur), standalone(des));
        for (name, c, d) in merge(&cur, &des) {
            let r = ObjectRef::new(ObjectKind::Index, [s, rel, name]);
            self.constraint(r, c, d, |x| x.comment = None, |x| (true, &x.comment));
        }
    }

    fn policies(&mut self, s: &str, cur: &Table, des: &Table) {
        let t = &des.name;
        for (name, c, d) in merge(&cur.policies, &des.policies) {
            let r = ObjectRef::new(ObjectKind::Policy, [s, t, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => {
                    if needs_policy_recreate(c, d) {
                        self.recreate(r, "command or kind changed");
                        continue;
                    }
                    let mut ops = Vec::new();
                    if c.roles != d.roles || c.using != d.using || c.check != d.check {
                        ops.push(AlterOp::PolicyDefinition);
                    }
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    /// Triggers of a table or view.
    pub(super) fn triggers(
        &mut self,
        s: &str,
        rel: &str,
        cur: &BTreeMap<String, Trigger>,
        des: &BTreeMap<String, Trigger>,
    ) {
        for (name, c, d) in merge(cur, des) {
            let r = ObjectRef::new(ObjectKind::Trigger, [s, rel, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => {
                    let mut ops = Vec::new();
                    let same = same_except(c, d, |x| {
                        x.comment = None;
                        x.enabled = Default::default();
                    });
                    if !same {
                        // `CREATE OR REPLACE` does not support constraint triggers.
                        if c.constraint.is_some() || d.constraint.is_some() {
                            self.recreate(r, "definition changed");
                            continue;
                        }
                        ops.push(AlterOp::Replace);
                    }
                    if c.enabled != d.enabled {
                        ops.push(AlterOp::Enabled);
                    }
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }
}

/// `ALTER POLICY` cannot change the command or kind, nor remove a `USING` or `WITH CHECK`
/// clause.
fn needs_policy_recreate(c: &Policy, d: &Policy) -> bool {
    c.command != d.command
        || c.permissive != d.permissive
        || (c.using.is_some() && d.using.is_none())
        || (c.check.is_some() && d.check.is_none())
}
