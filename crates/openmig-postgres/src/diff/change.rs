//! What [`super::diff`] produces.

use crate::model::{AclItem, QualifiedName};
use openmig_core::{ChangeInfo, ObjectKind, ObjectRef};
use std::collections::BTreeMap;

/// One difference between the current and the desired model.
///
/// Changes name objects by [`ObjectRef`] (paths as in [`crate::model::Database::get`]); the
/// planner looks their definitions up in the two models. `Create` and `Drop` cover the object
/// with everything nested in it (a table with its columns, constraints, indexes, policies,
/// triggers, comments and grants), so the diff never reports nested changes for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The object exists only in the desired model.
    Create(ObjectRef),
    /// The object exists only in the current model.
    Drop(ObjectRef),
    /// The object cannot be changed in place: drop the current definition and create the
    /// desired one. `destructive` when that loses data (rows of a table, values of a column).
    Recreate {
        object: ObjectRef,
        reason: String,
        destructive: bool,
    },
    /// In-place changes; values come from the desired model unless the op carries them.
    Alter {
        object: ObjectRef,
        ops: Vec<AlterOp>,
    },
    /// `from` (a current object) becomes `to`. Applied before every other change; the rest of
    /// the diff sees the current model with the new name.
    Rename { from: ObjectRef, to: ObjectRef },
    /// Rows of a data-managed table.
    Data {
        table: QualifiedName,
        rows: DataChanges,
    },
}

/// One in-place change of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlterOp {
    /// `COMMENT ON`.
    Comment,
    /// Privileges of managed grantees.
    Acl(AclDelta),

    /// Role attributes (`LOGIN`, `CONNECTION LIMIT`, ...).
    RoleAttributes,
    /// Membership in the named role: added, or its options changed.
    GrantMembership(String),
    /// Membership in the named role removed.
    RevokeMembership(String),

    /// `ALTER EXTENSION ... SET SCHEMA`.
    ExtensionSchema,

    /// `ENABLE` / `DISABLE ROW LEVEL SECURITY`.
    RowSecurity,
    /// `FORCE` / `NO FORCE ROW LEVEL SECURITY`.
    ForceRowSecurity,
    ReplicaIdentity,
    /// `SET LOGGED` / `SET UNLOGGED`.
    Persistence,
    /// `SET (...)` / `RESET (...)` of tables and materialized views.
    StorageParams,
    /// `ALTER TABLE ... INHERIT parent`.
    Inherit(QualifiedName),
    /// `ALTER TABLE ... NO INHERIT parent`.
    NoInherit(QualifiedName),
    /// Detach from the current partitioned parent and/or attach to the desired one.
    Partition,

    /// Column type or collation (`ALTER COLUMN ... TYPE`).
    Type,
    /// Column or domain `SET` / `DROP NOT NULL`.
    NotNull,
    /// Column or domain `SET` / `DROP DEFAULT`.
    Default,
    /// Add, drop or change a column's identity.
    Identity,
    /// `SET EXPRESSION` (PostgreSQL 17+) or `DROP EXPRESSION` of a generated column.
    Generated,
    /// Column `SET STORAGE`.
    Storage,
    /// Column `SET COMPRESSION`.
    Compression,

    /// `VALIDATE CONSTRAINT` of a constraint added `NOT VALID`.
    Validate,
    /// `ALTER POLICY` (roles, `USING`, `WITH CHECK`).
    PolicyDefinition,
    /// Trigger or event trigger firing mode.
    Enabled,
    /// `CREATE OR REPLACE` of a view, function, procedure or trigger.
    Replace,
    /// View options (`security_barrier`, `security_invoker`, `check_option`).
    ViewOptions,
    /// `REFRESH MATERIALIZED VIEW [WITH NO DATA]`.
    Populate,
    /// Function attributes that `ALTER FUNCTION` can change.
    Attributes,
    /// Sequence data type and parameters.
    SequenceOptions,
    /// `ALTER SEQUENCE ... OWNED BY`.
    OwnedBy,
    /// `ALTER TYPE ... ADD VALUE`.
    AddValue {
        value: String,
        position: EnumPosition,
    },
    /// `ALTER TYPE ... ADD ATTRIBUTE` (composite types).
    AddAttribute(String),
    /// `ALTER TYPE ... DROP ATTRIBUTE`.
    DropAttribute(String),
    /// `ALTER TYPE ... ALTER ATTRIBUTE ... TYPE`.
    AlterAttribute(String),
}

impl AlterOp {
    /// Short name for summaries.
    pub fn name(&self) -> String {
        match self {
            AlterOp::Comment => "comment".into(),
            AlterOp::Acl(_) => "privileges".into(),
            AlterOp::RoleAttributes => "attributes".into(),
            AlterOp::GrantMembership(r) => format!("grant membership in {r}"),
            AlterOp::RevokeMembership(r) => format!("revoke membership in {r}"),
            AlterOp::ExtensionSchema => "schema".into(),
            AlterOp::RowSecurity => "row level security".into(),
            AlterOp::ForceRowSecurity => "force row level security".into(),
            AlterOp::ReplicaIdentity => "replica identity".into(),
            AlterOp::Persistence => "logged".into(),
            AlterOp::StorageParams => "storage parameters".into(),
            AlterOp::Inherit(p) => format!("inherit {p}"),
            AlterOp::NoInherit(p) => format!("no inherit {p}"),
            AlterOp::Partition => "partition".into(),
            AlterOp::Type => "type".into(),
            AlterOp::NotNull => "not null".into(),
            AlterOp::Default => "default".into(),
            AlterOp::Identity => "identity".into(),
            AlterOp::Generated => "generated expression".into(),
            AlterOp::Storage => "storage".into(),
            AlterOp::Compression => "compression".into(),
            AlterOp::Validate => "validate".into(),
            AlterOp::PolicyDefinition => "definition".into(),
            AlterOp::Enabled => "enabled".into(),
            AlterOp::Replace => "definition".into(),
            AlterOp::ViewOptions => "options".into(),
            AlterOp::Populate => "populate".into(),
            AlterOp::Attributes => "attributes".into(),
            AlterOp::SequenceOptions => "options".into(),
            AlterOp::OwnedBy => "owned by".into(),
            AlterOp::AddValue { value, .. } => format!("add value {value}"),
            AlterOp::AddAttribute(a) => format!("add attribute {a}"),
            AlterOp::DropAttribute(a) => format!("drop attribute {a}"),
            AlterOp::AlterAttribute(a) => format!("alter attribute {a}"),
        }
    }
}

/// Where `ALTER TYPE ... ADD VALUE` puts a new label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnumPosition {
    Before(String),
    After(String),
    /// After the last label.
    End,
}

/// Privilege changes for managed grantees.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AclDelta {
    /// Privileges to grant; `grantable` items are granted `WITH GRANT OPTION`.
    pub grant: Vec<AclItem>,
    /// Privileges to revoke entirely.
    pub revoke: Vec<AclItem>,
    /// Privileges to keep but without the grant option.
    pub revoke_grant_option: Vec<AclItem>,
}

impl AclDelta {
    pub fn is_empty(&self) -> bool {
        self.grant.is_empty() && self.revoke.is_empty() && self.revoke_grant_option.is_empty()
    }
}

/// A row of a data-managed table: managed column to value (`None` for NULL).
pub type Row = BTreeMap<String, Option<String>>;

/// Row changes of one data-managed table, keyed by the values of `key`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataChanges {
    pub key: Vec<String>,
    pub insert: Vec<Row>,
    /// Key values and the columns to set.
    pub update: Vec<(Vec<Option<String>>, Row)>,
    pub delete: Vec<Vec<Option<String>>>,
}

impl DataChanges {
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.update.is_empty() && self.delete.is_empty()
    }
}

impl Change {
    /// The object the change is about (the new identity for renames).
    pub fn object_ref(&self) -> ObjectRef {
        match self {
            Change::Create(o) | Change::Drop(o) => o.clone(),
            Change::Recreate { object, .. } | Change::Alter { object, .. } => object.clone(),
            Change::Rename { to, .. } => to.clone(),
            Change::Data { table, .. } => {
                ObjectRef::new(ObjectKind::Data, [&table.schema, &table.name])
            }
        }
    }
}

/// Why dropping `object` destroys data.
pub(crate) fn drop_destroys(object: &ObjectRef) -> Option<String> {
    use ObjectKind as K;
    let what = match object.kind {
        K::Table => "drops table {} and its rows",
        K::Column => "drops column {} and its values",
        K::Schema => "drops schema {}",
        K::Sequence => "drops sequence {} and its current value",
        K::MaterializedView => "drops materialized view {} and its data",
        _ => return None,
    };
    Some(what.replace("{}", &object.dotted()))
}

impl ChangeInfo for Change {
    fn object(&self) -> ObjectRef {
        self.object_ref()
    }

    fn summary(&self) -> String {
        match self {
            Change::Create(o) => format!("create {o}"),
            Change::Drop(o) => format!("drop {o}"),
            Change::Recreate { object, reason, .. } => format!("recreate {object}: {reason}"),
            Change::Alter { object, ops } => {
                let ops: Vec<String> = ops.iter().map(AlterOp::name).collect();
                format!("alter {object}: {}", ops.join(", "))
            }
            Change::Rename { from, to } => format!("rename {from} to {}", to.dotted()),
            Change::Data { table, rows } => format!(
                "data {table}: {} to insert, {} to update, {} to delete",
                rows.insert.len(),
                rows.update.len(),
                rows.delete.len()
            ),
        }
    }

    fn destructive(&self) -> Option<String> {
        match self {
            Change::Drop(o) => drop_destroys(o),
            Change::Recreate {
                object,
                destructive: true,
                ..
            } => Some(format!("recreates {object} and loses its data")),
            Change::Data { table, rows } if !rows.delete.is_empty() => {
                Some(format!("deletes {} rows from {table}", rows.delete.len()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info() {
        let t = ObjectRef::new(ObjectKind::Table, ["app", "users"]);
        assert_eq!(
            Change::Create(t.clone()).summary(),
            "create table app.users"
        );
        assert!(Change::Drop(t.clone()).destructive().is_some());
        let c = ObjectRef::new(ObjectKind::Column, ["app", "users", "email"]);
        let alter = Change::Alter {
            object: c.clone(),
            ops: vec![AlterOp::Type, AlterOp::NotNull],
        };
        assert_eq!(
            alter.summary(),
            "alter column app.users.email: type, not null"
        );
        assert_eq!(alter.destructive(), None);
        assert!(
            Change::Drop(ObjectRef::new(ObjectKind::Index, ["app", "users", "i"]))
                .destructive()
                .is_none()
        );
    }
}
