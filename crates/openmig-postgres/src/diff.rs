//! Differences between two models.
//!
//! [`diff`] compares the current and the desired model object kind by object kind, in `BTreeMap`
//! order, and returns [`Change`]s that name objects by key. Renames from the [`RenameSet`] are
//! applied to the current model first ([`rename::rekey`]), so a rename combined with other changes
//! diffs as the rename plus those changes.
//!
//! Never compared: owners, `dependencies`, `server_major`, extension versions, sequence current
//! values, column order of tables, and privileges of grantees that are not managed
//! ([`crate::restrict::is_managed_grantee`]).

mod change;
mod cluster;
mod data;
mod relations;
pub(crate) mod rename;
mod routines;
mod tables;
mod types;

pub(crate) use change::drop_destroys;
pub use change::{AclDelta, AlterOp, Change, DataChanges, EnumPosition, Row};
pub use rename::{rename_candidates, resolve_hints};
pub(crate) use tables::inherited_columns;

use crate::PostgresEngine;
use crate::model::{Acl, AclItem, Database, Schema, default_acl};
use crate::restrict::is_managed_grantee;
use crate::settings::PgSettings;
use openmig_core::{DiffContext, Error, ObjectKind, ObjectRef, PlanMode, Scope};
use std::collections::{BTreeMap, BTreeSet};

/// Hint attached to unsupported changes.
pub(crate) const MANUAL_HINT: &str =
    "write this migration by hand with `openmig new`, or allow recreating with `sync --recreate`";

/// What `diff` and `plan` need from the [`DiffContext`], plus the server version that decides
/// version-dependent syntax.
pub(crate) struct Options<'a> {
    pub settings: &'a PgSettings,
    pub scope: &'a Scope,
    pub mode: PlanMode,
    pub allow_recreate: bool,
    /// Major version of the server the plan runs on: the current side's, or the desired side's
    /// when the current model was not inspected.
    pub major: u32,
}

impl<'a> Options<'a> {
    pub fn new(
        ctx: &DiffContext<'a, PostgresEngine>,
        current: &Database,
        desired: &Database,
    ) -> Self {
        Self {
            settings: ctx.settings,
            scope: ctx.scope,
            mode: ctx.mode,
            allow_recreate: ctx.allow_recreate,
            major: if current.server_major != 0 {
                current.server_major
            } else {
                desired.server_major
            },
        }
    }

    pub fn scratch(&self) -> bool {
        self.mode == PlanMode::Scratch
    }

    /// Roles are compared when they are managed, and always created when building a scratch
    /// database (grants and policies need them).
    pub fn roles_compared(&self) -> bool {
        self.scope.manage_roles || self.scratch()
    }
}

/// Compare `current` (with `ctx.renames` applied) to `desired`.
pub fn diff(
    current: &Database,
    desired: &Database,
    ctx: &DiffContext<'_, PostgresEngine>,
) -> Result<Vec<Change>, Error> {
    let opts = Options::new(ctx, current, desired);
    let renames = rename::applicable(current, ctx.renames);
    let rekeyed = rename::rekey(current, &renames);
    let mut d = Differ {
        cur: &rekeyed,
        des: desired,
        opts: &opts,
        new_schemas: rename::new_schemas(current, &renames),
        changes: renames
            .into_iter()
            .map(|(from, to)| Change::Rename { from, to })
            .collect(),
        errors: Vec::new(),
    };
    d.extensions();
    if opts.roles_compared() {
        d.roles();
    }
    d.schemas();
    d.event_triggers();
    d.data();
    match d.errors.into_iter().next() {
        Some(e) => Err(e),
        None => Ok(d.changes),
    }
}

pub(crate) struct Differ<'a> {
    pub cur: &'a Database,
    pub des: &'a Database,
    pub opts: &'a Options<'a>,
    /// Schemas that exist in `cur` only because renamed tables were moved into them.
    pub new_schemas: BTreeSet<String>,
    pub changes: Vec<Change>,
    pub errors: Vec<Error>,
}

impl Differ<'_> {
    pub fn push(&mut self, change: Change) {
        self.changes.push(change);
    }

    pub fn create(&mut self, object: ObjectRef) {
        self.changes.push(Change::Create(object));
    }

    /// Drops are never planned when building a scratch database: it starts with nothing but the
    /// `public` schema, which stays.
    pub fn drop(&mut self, object: ObjectRef) {
        if !self.opts.scratch() {
            self.changes.push(Change::Drop(object));
        }
    }

    pub fn alter(&mut self, object: ObjectRef, ops: Vec<AlterOp>) {
        if !ops.is_empty() {
            self.changes.push(Change::Alter { object, ops });
        }
    }

    /// Drop and create `object` again without losing data.
    pub fn recreate(&mut self, object: ObjectRef, reason: impl Into<String>) {
        self.changes.push(Change::Recreate {
            object,
            reason: reason.into(),
            destructive: false,
        });
    }

    /// A change PostgreSQL cannot make in place: recreate (losing data) when allowed, else an
    /// error.
    pub fn unsupported(&mut self, object: ObjectRef, reason: impl Into<String>) {
        let reason = reason.into();
        if self.opts.allow_recreate {
            self.changes.push(Change::Recreate {
                object,
                reason,
                destructive: true,
            });
        } else {
            self.errors.push(Error::Unsupported {
                object,
                reason,
                hint: Some(MANUAL_HINT.to_owned()),
            });
        }
    }

    /// `Comment` when the comments differ.
    pub fn comment_op(ops: &mut Vec<AlterOp>, cur: &Option<String>, des: &Option<String>) {
        if cur != des {
            ops.push(AlterOp::Comment);
        }
    }

    /// `Acl` when the managed privileges differ.
    pub fn acl_op(&self, ops: &mut Vec<AlterOp>, cur: &Acl, des: &Acl) {
        if let Some(delta) = acl_delta(cur, des, self.opts.scope, self.opts.major) {
            ops.push(AlterOp::Acl(delta));
        }
    }

    fn schemas(&mut self) {
        for (name, cur, des) in merge(&self.cur.schemas, &self.des.schemas) {
            let r = ObjectRef::new(ObjectKind::Schema, [name]);
            match (cur, des) {
                (None, Some(des)) => {
                    self.create(r);
                    let created = Schema {
                        name: name.clone(),
                        ..Schema::default()
                    };
                    self.schema_contents(&created, des);
                }
                (Some(cur), None) => {
                    let gone = Schema {
                        name: name.clone(),
                        ..Schema::default()
                    };
                    self.schema_contents(cur, &gone);
                    self.drop(r);
                }
                (Some(cur), Some(des)) => {
                    if self.new_schemas.contains(name) {
                        self.create(r);
                    } else {
                        let mut ops = Vec::new();
                        Self::comment_op(&mut ops, &cur.comment, &des.comment);
                        self.acl_op(&mut ops, &cur.acl, &des.acl);
                        self.alter(r, ops);
                    }
                    self.schema_contents(cur, des);
                }
                (None, None) => {}
            }
        }
    }

    fn schema_contents(&mut self, cur: &Schema, des: &Schema) {
        self.tables(cur, des);
        self.views(cur, des);
        self.materialized_views(cur, des);
        self.routines(cur, des, ObjectKind::Function);
        self.routines(cur, des, ObjectKind::Procedure);
        self.sequences(cur, des);
        self.enums(cur, des);
        self.composites(cur, des);
        self.domains(cur, des);
    }
}

/// Keys of two maps in order, with the value on each side.
pub(crate) fn merge<'m, K: Ord, V>(
    cur: &'m BTreeMap<K, V>,
    des: &'m BTreeMap<K, V>,
) -> Vec<(&'m K, Option<&'m V>, Option<&'m V>)> {
    let keys: BTreeSet<&K> = cur.keys().chain(des.keys()).collect();
    keys.into_iter()
        .map(|k| (k, cur.get(k), des.get(k)))
        .collect()
}

/// The managed part of an ACL, by `(grantee, privilege)`. `MAINTAIN` only exists from
/// PostgreSQL 17 on.
fn managed(
    acl: &Acl,
    scope: &Scope,
    major: u32,
) -> BTreeMap<(String, crate::model::Privilege), bool> {
    acl.iter()
        .filter(|i| is_managed_grantee(&i.grantee, scope))
        .filter(|i| i.privilege != crate::model::Privilege::Maintain || major >= 17)
        .map(|i| ((i.grantee.clone(), i.privilege), i.grantable))
        .collect()
}

/// Grants and revokes that turn the managed part of `cur` into that of `des` on a server of
/// version `major`.
pub(crate) fn acl_delta(cur: &Acl, des: &Acl, scope: &Scope, major: u32) -> Option<AclDelta> {
    let cur = managed(cur, scope, major);
    let des = managed(des, scope, major);
    let mut delta = AclDelta::default();
    for ((grantee, privilege), &grantable) in &des {
        let item = AclItem {
            grantee: grantee.clone(),
            privilege: *privilege,
            grantable,
        };
        match cur.get(&(grantee.clone(), *privilege)) {
            None => delta.grant.push(item),
            Some(false) if grantable => delta.grant.push(item),
            Some(true) if !grantable => delta.revoke_grant_option.push(item),
            Some(_) => {}
        }
    }
    for ((grantee, privilege), &grantable) in &cur {
        if !des.contains_key(&(grantee.clone(), *privilege)) {
            delta.revoke.push(AclItem {
                grantee: grantee.clone(),
                privilege: *privilege,
                grantable,
            });
        }
    }
    (!delta.is_empty()).then_some(delta)
}

/// Privileges to apply to a freshly created object of `kind`, which starts with PostgreSQL's
/// default ACL.
pub(crate) fn acl_for_new(
    kind: ObjectKind,
    des: &Acl,
    scope: &Scope,
    major: u32,
) -> Option<AclDelta> {
    acl_delta(&default_acl(kind), des, scope, major)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PUBLIC, Privilege};

    #[test]
    fn acl_deltas() {
        let scope = Scope {
            declared_roles: vec!["web".into(), "admin".into()],
            ..Scope::default()
        };
        let mut cur = Acl::new();
        cur.insert(AclItem::new(PUBLIC, Privilege::Execute));
        cur.insert(AclItem {
            grantee: "admin".into(),
            privilege: Privilege::Execute,
            grantable: true,
        });
        cur.insert(AclItem::new("unmanaged", Privilege::Execute));
        let mut des = Acl::new();
        des.insert(AclItem {
            grantee: "web".into(),
            privilege: Privilege::Execute,
            grantable: true,
        });
        des.insert(AclItem::new("admin", Privilege::Execute));
        let d = acl_delta(&cur, &des, &scope, 17).unwrap();
        assert_eq!(
            d.grant,
            vec![AclItem {
                grantee: "web".into(),
                privilege: Privilege::Execute,
                grantable: true
            }]
        );
        assert_eq!(d.revoke, vec![AclItem::new(PUBLIC, Privilege::Execute)]);
        assert_eq!(
            d.revoke_grant_option,
            vec![AclItem::new("admin", Privilege::Execute)]
        );
        assert_eq!(acl_delta(&cur, &cur, &scope, 17), None);
        assert_eq!(
            acl_for_new(
                ObjectKind::Function,
                &default_acl(ObjectKind::Function),
                &scope,
                17
            ),
            None
        );
        let maintain = Acl::from([AclItem::new("web", Privilege::Maintain)]);
        assert!(acl_delta(&Acl::new(), &maintain, &scope, 17).is_some());
        assert_eq!(acl_delta(&Acl::new(), &maintain, &scope, 16), None);
    }
}
