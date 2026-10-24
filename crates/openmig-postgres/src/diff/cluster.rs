//! Extensions, roles and event triggers.

use super::{AlterOp, Differ, merge};
use crate::model::{Membership, Role};
use openmig_core::{ObjectKind, ObjectRef};

impl Differ<'_> {
    /// Extension versions are not compared: the desired side reports whatever version the
    /// scratch server installs by default (decoded models only carry a declared version, which
    /// is used when the extension is created).
    pub(super) fn extensions(&mut self) {
        for (name, cur, des) in merge(&self.cur.extensions, &self.des.extensions) {
            let r = ObjectRef::new(ObjectKind::Extension, [name]);
            match (cur, des) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(cur), Some(des)) => {
                    let mut ops = Vec::new();
                    if cur.schema != des.schema {
                        ops.push(AlterOp::ExtensionSchema);
                    }
                    // `CREATE EXTENSION` sets the comment of the extension's control file; a
                    // comment is only managed when the desired side has one.
                    if des.comment.is_some() {
                        Self::comment_op(&mut ops, &cur.comment, &des.comment);
                    }
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    pub(super) fn roles(&mut self) {
        for (name, cur, des) in merge(&self.cur.roles, &self.des.roles) {
            let r = ObjectRef::new(ObjectKind::Role, [name]);
            match (cur, des) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => {
                    if self.opts.scope.manage_roles {
                        self.drop(r);
                    }
                }
                (Some(cur), Some(des)) => {
                    let mut ops = Vec::new();
                    if attributes_differ(cur, des) {
                        ops.push(AlterOp::RoleAttributes);
                    }
                    for (role, c, d) in merge(&cur.member_of, &des.member_of) {
                        match (c, d) {
                            (None, Some(_)) => ops.push(AlterOp::GrantMembership(role.clone())),
                            (Some(_), None) => ops.push(AlterOp::RevokeMembership(role.clone())),
                            (Some(c), Some(d)) if !self.same_membership(c, d) => {
                                ops.push(AlterOp::GrantMembership(role.clone()));
                            }
                            _ => {}
                        }
                    }
                    Self::comment_op(&mut ops, &cur.comment, &des.comment);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    /// Before PostgreSQL 16 only the admin option is a property of the membership.
    pub(super) fn same_membership(&self, a: &Membership, b: &Membership) -> bool {
        if self.opts.major >= 16 {
            a == b
        } else {
            a.admin == b.admin
        }
    }

    pub(super) fn event_triggers(&mut self) {
        for (name, cur, des) in merge(&self.cur.event_triggers, &self.des.event_triggers) {
            let r = ObjectRef::new(ObjectKind::EventTrigger, [name]);
            match (cur, des) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(cur), Some(des)) => {
                    if cur.event != des.event
                        || cur.tags != des.tags
                        || cur.function != des.function
                    {
                        self.recreate(r, "event, tags or function changed");
                        continue;
                    }
                    let mut ops = Vec::new();
                    if cur.enabled != des.enabled {
                        ops.push(AlterOp::Enabled);
                    }
                    Self::comment_op(&mut ops, &cur.comment, &des.comment);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }
}

/// `VALID UNTIL`, with `infinity` and no expiry being the same: an expiry can be set to
/// `infinity` but not removed.
fn expiry(r: &Role) -> Option<&str> {
    r.valid_until.as_deref().filter(|v| *v != "infinity")
}

fn attributes_differ(a: &Role, b: &Role) -> bool {
    a.login != b.login
        || a.superuser != b.superuser
        || a.createdb != b.createdb
        || a.createrole != b.createrole
        || a.inherit != b.inherit
        || a.replication != b.replication
        || a.bypassrls != b.bypassrls
        || a.connection_limit != b.connection_limit
        || expiry(a) != expiry(b)
}
