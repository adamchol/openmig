//! Changes between two models (see `docs/spec/architecture.md`, "ClickHouse planning rules").
//!
//! [`diff`] compares two models object kind by object kind, keyed by identity, and produces
//! [`Change`]s. Tables are altered in place where ClickHouse allows it; changes that cannot be
//! altered (engine, partition key, primary key, sorting key except appending new columns, the
//! type of a key column) are [`Error::Unsupported`] unless recreating is allowed. Objects that
//! block dropping or renaming a table (materialized views and dictionaries reading from it) are
//! dropped and recreated around it.

mod table;

use crate::ClickHouseEngine;
use crate::model::{
    Column, ColumnDefault, Constraint, Database, DefaultKind, Dictionary, Function, Grant, Grantee,
    Index, MaterializedView, Model, Projection, QualifiedName, Role, RoleGrant, RowPolicy,
    RowPolicyId, Table, View,
};
use openmig_core::{ChangeInfo, DiffContext, Error, ObjectKind, ObjectRef, PlanMode};
use std::collections::{BTreeMap, BTreeSet};

pub use table::diff_table;

/// One change to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    CreateDatabase(Database),
    DropDatabase(Database),
    /// `ALTER DATABASE ... MODIFY COMMENT`.
    CommentDatabase {
        name: String,
        comment: Option<String>,
    },
    CreateTable {
        database: String,
        table: Table,
    },
    DropTable {
        database: String,
        table: Table,
        /// Dropped to be created again (recreate, or a dependant of a recreated table).
        recreate: bool,
    },
    RenameTable {
        from: QualifiedName,
        to: QualifiedName,
    },
    AlterTable {
        database: String,
        table: String,
        ops: Vec<TableOp>,
    },
    /// `ALTER TABLE ... DROP COLUMN`, planned after the views that stop using the columns
    /// (ClickHouse refuses to drop a column a materialized view references).
    DropColumns {
        database: String,
        table: String,
        columns: Vec<Column>,
    },
    /// `CREATE OR REPLACE VIEW`.
    CreateView {
        database: String,
        view: View,
        replace: bool,
    },
    DropView {
        database: String,
        view: View,
        recreate: bool,
    },
    CreateMaterializedView {
        database: String,
        view: MaterializedView,
    },
    DropMaterializedView {
        database: String,
        view: MaterializedView,
        recreate: bool,
    },
    /// `ALTER TABLE mv MODIFY QUERY`.
    ModifyQuery {
        database: String,
        view: String,
        query: String,
    },
    /// `ALTER TABLE mv MODIFY COMMENT`.
    CommentMaterializedView {
        database: String,
        view: String,
        comment: Option<String>,
    },
    /// `CREATE OR REPLACE DICTIONARY`.
    CreateDictionary {
        database: String,
        dictionary: Dictionary,
        replace: bool,
    },
    DropDictionary {
        database: String,
        dictionary: Dictionary,
        recreate: bool,
    },
    /// `CREATE OR REPLACE FUNCTION`.
    CreateFunction {
        function: Function,
        replace: bool,
    },
    DropFunction(Function),
    CreateRole(Role),
    DropRole {
        role: Role,
        recreate: bool,
    },
    Grant(Grant),
    Revoke(Grant),
    /// `REVOKE GRANT OPTION FOR`: the privileges stay.
    RevokeGrantOption(Grant),
    GrantRole(RoleGrant),
    RevokeRole(RoleGrant),
    /// `REVOKE ADMIN OPTION FOR`: the membership stays.
    RevokeAdminOption(RoleGrant),
    /// `CREATE ROW POLICY OR REPLACE`.
    CreateRowPolicy {
        id: RowPolicyId,
        policy: RowPolicy,
        replace: bool,
    },
    DropRowPolicy {
        id: RowPolicyId,
        policy: RowPolicy,
    },
}

/// One `ALTER TABLE` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableOp {
    /// `ADD COLUMN ... AFTER after` (`FIRST` when `None`).
    AddColumn {
        column: Column,
        after: Option<String>,
    },
    DropColumn(Column),
    RenameColumn {
        from: String,
        to: String,
    },
    ModifyColumn {
        column: String,
        change: ColumnChange,
    },
    AddIndex {
        name: String,
        index: Index,
    },
    DropIndex(String),
    AddProjection {
        name: String,
        projection: Projection,
    },
    DropProjection(String),
    AddConstraint {
        name: String,
        constraint: Constraint,
    },
    DropConstraint(String),
    /// Appends newly added columns to the sorting key; issued in the same `ALTER` as their
    /// `ADD COLUMN`s.
    ModifyOrderBy(Vec<String>),
    ModifySampleBy(String),
    RemoveSampleBy,
    ModifyTtl(String),
    RemoveTtl,
    ModifySettings(BTreeMap<String, String>),
    ResetSettings(Vec<String>),
    ModifyComment(Option<String>),
}

/// One property of a column changed by `MODIFY COLUMN`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnChange {
    Type(String),
    Default(ColumnDefault),
    RemoveDefault(DefaultKind),
    Codec(Vec<String>),
    RemoveCodec,
    Ttl(String),
    RemoveTtl,
    Comment(String),
    RemoveComment,
}

fn table_ref(database: &str, name: &str) -> ObjectRef {
    ObjectRef::new(ObjectKind::Table, [database, name])
}

impl ChangeInfo for Change {
    fn object(&self) -> ObjectRef {
        use ObjectKind as K;
        match self {
            Change::CreateDatabase(db) | Change::DropDatabase(db) => {
                ObjectRef::new(K::Database, [db.name.as_str()])
            }
            Change::CommentDatabase { name, .. } => ObjectRef::new(K::Database, [name.as_str()]),
            Change::CreateTable { database, table }
            | Change::DropTable {
                database, table, ..
            } => table_ref(database, &table.name),
            Change::RenameTable { to, .. } => table_ref(&to.database, &to.name),
            Change::AlterTable {
                database, table, ..
            }
            | Change::DropColumns {
                database, table, ..
            } => table_ref(database, table),
            Change::CreateView { database, view, .. } | Change::DropView { database, view, .. } => {
                ObjectRef::new(K::View, [database, &view.name])
            }
            Change::CreateMaterializedView { database, view }
            | Change::DropMaterializedView { database, view, .. } => {
                ObjectRef::new(K::MaterializedView, [database, &view.name])
            }
            Change::ModifyQuery { database, view, .. }
            | Change::CommentMaterializedView { database, view, .. } => {
                ObjectRef::new(K::MaterializedView, [database, view])
            }
            Change::CreateDictionary {
                database,
                dictionary,
                ..
            }
            | Change::DropDictionary {
                database,
                dictionary,
                ..
            } => ObjectRef::new(K::Dictionary, [database, &dictionary.name]),
            Change::CreateFunction { function, .. } | Change::DropFunction(function) => {
                ObjectRef::new(K::Function, [function.name.as_str()])
            }
            Change::CreateRole(role) | Change::DropRole { role, .. } => {
                ObjectRef::new(K::Role, [role.name.as_str()])
            }
            Change::Grant(g) | Change::Revoke(g) | Change::RevokeGrantOption(g) => g.object_ref(),
            Change::GrantRole(g) | Change::RevokeRole(g) | Change::RevokeAdminOption(g) => {
                g.grantee.object_ref()
            }
            Change::CreateRowPolicy { id, .. } | Change::DropRowPolicy { id, .. } => {
                id.object_ref()
            }
        }
    }

    fn summary(&self) -> String {
        let object = self.object();
        let what = object.dotted();
        match self {
            Change::CreateDatabase(_) => format!("create database {what}"),
            Change::DropDatabase(_) => format!("drop database {what}"),
            Change::CommentDatabase { .. } => format!("comment on database {what}"),
            Change::CreateTable { .. } => format!("create table {what}"),
            Change::DropTable { recreate: true, .. } => format!("drop table {what} (recreated)"),
            Change::DropTable { .. } => format!("drop table {what}"),
            Change::RenameTable { from, .. } => format!("rename table {from} to {what}"),
            Change::AlterTable { ops, .. } => {
                let ops: Vec<String> = ops.iter().map(TableOp::summary).collect();
                format!("alter table {what}: {}", ops.join(", "))
            }
            Change::DropColumns { columns, .. } => {
                let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
                format!("alter table {what}: drop column {}", names.join(", "))
            }
            Change::CreateView { replace: true, .. } => format!("replace view {what}"),
            Change::CreateView { .. } => format!("create view {what}"),
            Change::DropView { recreate: true, .. } => format!("drop view {what} (recreated)"),
            Change::DropView { .. } => format!("drop view {what}"),
            Change::CreateMaterializedView { .. } => format!("create materialized view {what}"),
            Change::DropMaterializedView { recreate: true, .. } => {
                format!("drop materialized view {what} (recreated)")
            }
            Change::DropMaterializedView { .. } => format!("drop materialized view {what}"),
            Change::ModifyQuery { .. } => format!("modify query of materialized view {what}"),
            Change::CommentMaterializedView { .. } => {
                format!("comment on materialized view {what}")
            }
            Change::CreateDictionary { replace: true, .. } => format!("replace dictionary {what}"),
            Change::CreateDictionary { .. } => format!("create dictionary {what}"),
            Change::DropDictionary { recreate: true, .. } => {
                format!("drop dictionary {what} (recreated)")
            }
            Change::DropDictionary { .. } => format!("drop dictionary {what}"),
            Change::CreateFunction { replace: true, .. } => format!("replace function {what}"),
            Change::CreateFunction { .. } => format!("create function {what}"),
            Change::DropFunction(_) => format!("drop function {what}"),
            Change::CreateRole(_) => format!("create role {what}"),
            Change::DropRole { .. } => format!("drop role {what}"),
            Change::Grant(g) => format!(
                "grant {} on {} to {}",
                privilege_list(g),
                g.target_sql(),
                g.grantee
            ),
            Change::Revoke(g) => format!(
                "revoke {} on {} from {}",
                privilege_list(g),
                g.target_sql(),
                g.grantee
            ),
            Change::RevokeGrantOption(g) => format!(
                "revoke grant option for {} on {} from {}",
                privilege_list(g),
                g.target_sql(),
                g.grantee
            ),
            Change::GrantRole(g) => format!("grant role {} to {}", g.role, g.grantee),
            Change::RevokeAdminOption(g) => {
                format!("revoke admin option for role {} from {}", g.role, g.grantee)
            }
            Change::RevokeRole(g) => format!("revoke role {} from {}", g.role, g.grantee),
            Change::CreateRowPolicy { replace: true, .. } => format!("replace row policy {what}"),
            Change::CreateRowPolicy { .. } => format!("create row policy {what}"),
            Change::DropRowPolicy { .. } => format!("drop row policy {what}"),
        }
    }

    fn destructive(&self) -> Option<String> {
        let what = self.object().dotted();
        match self {
            Change::DropDatabase(_) => Some(format!(
                "drops database {what} and every table in it with its rows"
            )),
            Change::DropTable { .. } => Some(format!("drops table {what} and its rows")),
            Change::DropMaterializedView { view, .. } if view.inner.is_some() => Some(format!(
                "drops materialized view {what} and the rows of its inner table"
            )),
            Change::DropColumns { columns, .. } => {
                let dropped: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
                Some(format!(
                    "drops column{} {} of {what} and their values",
                    if dropped.len() > 1 { "s" } else { "" },
                    dropped.join(", ")
                ))
            }
            _ => None,
        }
    }
}

fn privilege_list(grant: &Grant) -> String {
    let privileges: Vec<&str> = grant.privileges.iter().map(String::as_str).collect();
    if grant.columns.is_empty() {
        privileges.join(", ")
    } else {
        format!("{}({})", privileges.join(", "), grant.columns.join(", "))
    }
}

impl TableOp {
    pub fn summary(&self) -> String {
        match self {
            TableOp::AddColumn { column, .. } => format!("add column {}", column.name),
            TableOp::DropColumn(column) => format!("drop column {}", column.name),
            TableOp::RenameColumn { from, to } => format!("rename column {from} to {to}"),
            TableOp::ModifyColumn { column, change } => match change {
                ColumnChange::Type(t) => format!("column {column} type {t}"),
                ColumnChange::Default(d) => format!("column {column} {}", d.kind),
                ColumnChange::RemoveDefault(kind) => format!("column {column} drop {kind}"),
                ColumnChange::Codec(_) => format!("column {column} codec"),
                ColumnChange::RemoveCodec => format!("column {column} drop codec"),
                ColumnChange::Ttl(_) => format!("column {column} TTL"),
                ColumnChange::RemoveTtl => format!("column {column} drop TTL"),
                ColumnChange::Comment(_) | ColumnChange::RemoveComment => {
                    format!("column {column} comment")
                }
            },
            TableOp::AddIndex { name, .. } => format!("add index {name}"),
            TableOp::DropIndex(name) => format!("drop index {name}"),
            TableOp::AddProjection { name, .. } => format!("add projection {name}"),
            TableOp::DropProjection(name) => format!("drop projection {name}"),
            TableOp::AddConstraint { name, .. } => format!("add constraint {name}"),
            TableOp::DropConstraint(name) => format!("drop constraint {name}"),
            TableOp::ModifyOrderBy(_) => "order by".to_owned(),
            TableOp::ModifySampleBy(_) => "sample by".to_owned(),
            TableOp::RemoveSampleBy => "drop sample by".to_owned(),
            TableOp::ModifyTtl(_) => "TTL".to_owned(),
            TableOp::RemoveTtl => "drop TTL".to_owned(),
            TableOp::ModifySettings(s) => {
                format!(
                    "settings {}",
                    s.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
            TableOp::ResetSettings(s) => format!("reset settings {}", s.join(", ")),
            TableOp::ModifyComment(_) => "comment".to_owned(),
        }
    }
}

const RECREATE_HINT: &str = "write the migration by hand with `openmig new`, or run `openmig sync \
     --recreate` to drop and recreate the object (a table loses its rows)";

fn unsupported(object: ObjectRef, reason: String) -> Error {
    Error::Unsupported {
        object,
        reason,
        hint: Some(RECREATE_HINT.to_owned()),
    }
}

/// Compare `current` with `desired`.
pub fn diff(
    current: &Model,
    desired: &Model,
    ctx: &DiffContext<'_, ClickHouseEngine>,
) -> Result<Vec<Change>, Error> {
    let mut current = current.clone();
    let mut changes = Vec::new();
    let mut renamed_columns: BTreeMap<(String, String), Vec<TableOp>> = BTreeMap::new();
    apply_renames(&mut current, ctx, &mut changes, &mut renamed_columns);

    let recreated_roles = roles(&current, desired, ctx, &mut changes)?;
    functions(&current, desired, &mut changes);
    databases(&current, desired, ctx, &mut changes, &mut renamed_columns)?;
    grants(&current, desired, &recreated_roles, &mut changes);
    role_grants(&current, desired, &recreated_roles, &mut changes);
    row_policies(&current, desired, &mut changes);
    if ctx.mode == PlanMode::Scratch {
        for name in &desired.external_roles {
            if !current.roles.contains_key(name) && !desired.roles.contains_key(name) {
                changes.push(Change::CreateRole(Role {
                    name: name.clone(),
                    storage: String::new(),
                }));
            }
        }
    }
    recreate_blocking_dependants(&current, desired, &mut changes);
    Ok(changes)
}

/// Re-key renamed tables and columns of `current` and record the renames.
fn apply_renames(
    current: &mut Model,
    ctx: &DiffContext<'_, ClickHouseEngine>,
    changes: &mut Vec<Change>,
    renamed_columns: &mut BTreeMap<(String, String), Vec<TableOp>>,
) {
    for (from, to) in ctx.renames.of_kind(ObjectKind::Table) {
        let ([fd, fname], [td, tname]) = (from.path.as_slice(), to.path.as_slice()) else {
            continue;
        };
        let Some(mut table) = current
            .databases
            .get_mut(fd)
            .and_then(|db| db.tables.remove(fname))
        else {
            continue;
        };
        table.name.clone_from(tname);
        current
            .databases
            .entry(td.clone())
            .or_insert_with(|| Database::new(td, "Atomic"))
            .tables
            .insert(tname.clone(), table);
        changes.push(Change::RenameTable {
            from: QualifiedName::new(fd, fname),
            to: QualifiedName::new(td, tname),
        });
    }
    for (from, to) in ctx.renames.of_kind(ObjectKind::Column) {
        let ([_, _, old], [td, tt, new]) = (from.path.as_slice(), to.path.as_slice()) else {
            continue;
        };
        let Some(table) = current
            .databases
            .get_mut(td)
            .and_then(|db| db.tables.get_mut(tt))
        else {
            continue;
        };
        let Some(column) = table.columns.iter_mut().find(|c| &c.name == old) else {
            continue;
        };
        column.name.clone_from(new);
        renamed_columns
            .entry((td.clone(), tt.clone()))
            .or_default()
            .push(TableOp::RenameColumn {
                from: old.clone(),
                to: new.clone(),
            });
    }
}

fn roles(
    current: &Model,
    desired: &Model,
    ctx: &DiffContext<'_, ClickHouseEngine>,
    changes: &mut Vec<Change>,
) -> Result<BTreeSet<String>, Error> {
    let mut recreated = BTreeSet::new();
    for (name, role) in &current.roles {
        match desired.roles.get(name) {
            None => changes.push(Change::DropRole {
                role: role.clone(),
                recreate: false,
            }),
            Some(want) if !want.storage.is_empty() && want.storage != role.storage => {
                if !ctx.allow_recreate {
                    return Err(unsupported(
                        ObjectRef::new(ObjectKind::Role, [name.as_str()]),
                        format!(
                            "the role is stored in `{}`, the schema wants `{}`",
                            role.storage, want.storage
                        ),
                    ));
                }
                recreated.insert(name.clone());
                changes.push(Change::DropRole {
                    role: role.clone(),
                    recreate: true,
                });
                changes.push(Change::CreateRole(want.clone()));
            }
            Some(_) => {}
        }
    }
    for (name, role) in &desired.roles {
        if !current.roles.contains_key(name) {
            changes.push(Change::CreateRole(role.clone()));
        }
    }
    Ok(recreated)
}

fn functions(current: &Model, desired: &Model, changes: &mut Vec<Change>) {
    for (name, function) in &current.functions {
        if !desired.functions.contains_key(name) {
            changes.push(Change::DropFunction(function.clone()));
        }
    }
    for (name, function) in &desired.functions {
        match current.functions.get(name) {
            None => changes.push(Change::CreateFunction {
                function: function.clone(),
                replace: false,
            }),
            Some(old) if old != function => changes.push(Change::CreateFunction {
                function: function.clone(),
                replace: true,
            }),
            Some(_) => {}
        }
    }
}

fn databases(
    current: &Model,
    desired: &Model,
    ctx: &DiffContext<'_, ClickHouseEngine>,
    changes: &mut Vec<Change>,
    renamed_columns: &mut BTreeMap<(String, String), Vec<TableOp>>,
) -> Result<(), Error> {
    let empty = Database::default();
    for (name, db) in &current.databases {
        if !desired.databases.contains_key(name) {
            changes.push(Change::DropDatabase(db.clone()));
        }
    }
    for (name, want) in &desired.databases {
        let have = match current.databases.get(name) {
            None => {
                changes.push(Change::CreateDatabase(want.clone()));
                &empty
            }
            Some(have) if have.engine != want.engine => {
                if !ctx.allow_recreate {
                    return Err(unsupported(
                        ObjectRef::new(ObjectKind::Database, [name.as_str()]),
                        format!("the engine changes from {} to {}", have.engine, want.engine),
                    ));
                }
                changes.push(Change::DropDatabase(have.clone()));
                changes.push(Change::CreateDatabase(want.clone()));
                &empty
            }
            Some(have) => {
                if have.comment != want.comment {
                    changes.push(Change::CommentDatabase {
                        name: name.clone(),
                        comment: want.comment.clone(),
                    });
                }
                have
            }
        };
        database_objects(name, have, want, ctx, changes, renamed_columns)?;
    }
    Ok(())
}

fn database_objects(
    database: &str,
    have: &Database,
    want: &Database,
    ctx: &DiffContext<'_, ClickHouseEngine>,
    changes: &mut Vec<Change>,
    renamed_columns: &mut BTreeMap<(String, String), Vec<TableOp>>,
) -> Result<(), Error> {
    let db = database.to_owned();
    for (name, table) in &have.tables {
        if !want.tables.contains_key(name) {
            changes.push(Change::DropTable {
                database: db.clone(),
                table: table.clone(),
                recreate: false,
            });
        }
    }
    for (name, table) in &want.tables {
        let Some(old) = have.tables.get(name) else {
            changes.push(Change::CreateTable {
                database: db.clone(),
                table: table.clone(),
            });
            continue;
        };
        let renames = renamed_columns
            .remove(&(db.clone(), name.clone()))
            .unwrap_or_default();
        match diff_table(old, table) {
            Ok(ops) => {
                let (drops, ops): (Vec<TableOp>, Vec<TableOp>) = renames
                    .into_iter()
                    .chain(ops)
                    .partition(|op| matches!(op, TableOp::DropColumn(_)));
                if !ops.is_empty() {
                    changes.push(Change::AlterTable {
                        database: db.clone(),
                        table: name.clone(),
                        ops,
                    });
                }
                let columns: Vec<Column> = drops
                    .into_iter()
                    .filter_map(|op| match op {
                        TableOp::DropColumn(c) => Some(c),
                        _ => None,
                    })
                    .collect();
                if !columns.is_empty() {
                    changes.push(Change::DropColumns {
                        database: db.clone(),
                        table: name.clone(),
                        columns,
                    });
                }
            }
            Err(reason) if ctx.allow_recreate => {
                tracing::debug!(table = %name, %reason, "recreating table");
                changes.push(Change::DropTable {
                    database: db.clone(),
                    table: old.clone(),
                    recreate: true,
                });
                changes.push(Change::CreateTable {
                    database: db.clone(),
                    table: table.clone(),
                });
            }
            Err(reason) => return Err(unsupported(table_ref(&db, name), reason)),
        }
    }

    for (name, view) in &have.views {
        if !want.views.contains_key(name) {
            changes.push(Change::DropView {
                database: db.clone(),
                view: view.clone(),
                recreate: false,
            });
        }
    }
    for (name, view) in &want.views {
        match have.views.get(name) {
            None => changes.push(Change::CreateView {
                database: db.clone(),
                view: view.clone(),
                replace: false,
            }),
            Some(old) if old != view => changes.push(Change::CreateView {
                database: db.clone(),
                view: view.clone(),
                replace: true,
            }),
            Some(_) => {}
        }
    }

    for (name, mv) in &have.materialized_views {
        if !want.materialized_views.contains_key(name) {
            changes.push(Change::DropMaterializedView {
                database: db.clone(),
                view: mv.clone(),
                recreate: false,
            });
        }
    }
    for (name, mv) in &want.materialized_views {
        let Some(old) = have.materialized_views.get(name) else {
            changes.push(Change::CreateMaterializedView {
                database: db.clone(),
                view: mv.clone(),
            });
            continue;
        };
        if old == mv {
            continue;
        }
        // `MODIFY QUERY` also replaces the column list with the new query's columns.
        let same_except = |query: bool, comment: bool| {
            let mut a = old.clone();
            if query {
                a.query.clone_from(&mv.query);
                a.columns.clone_from(&mv.columns);
            }
            if comment {
                a.comment.clone_from(&mv.comment);
            }
            &a == mv
        };
        if old.to.is_some() && old.query != mv.query && same_except(true, true) {
            if old.query != mv.query {
                changes.push(Change::ModifyQuery {
                    database: db.clone(),
                    view: name.clone(),
                    query: mv.query.clone(),
                });
            }
            if old.comment != mv.comment {
                changes.push(Change::CommentMaterializedView {
                    database: db.clone(),
                    view: name.clone(),
                    comment: mv.comment.clone(),
                });
            }
        } else if same_except(false, true) {
            changes.push(Change::CommentMaterializedView {
                database: db.clone(),
                view: name.clone(),
                comment: mv.comment.clone(),
            });
        } else {
            if old.inner.is_some() && !ctx.allow_recreate {
                return Err(unsupported(
                    ObjectRef::new(ObjectKind::MaterializedView, [&db, name]),
                    "recreating a materialized view without `TO` loses the rows of its inner \
                     table"
                        .to_owned(),
                ));
            }
            changes.push(Change::DropMaterializedView {
                database: db.clone(),
                view: old.clone(),
                recreate: true,
            });
            changes.push(Change::CreateMaterializedView {
                database: db.clone(),
                view: mv.clone(),
            });
        }
    }

    for (name, dictionary) in &have.dictionaries {
        if !want.dictionaries.contains_key(name) {
            changes.push(Change::DropDictionary {
                database: db.clone(),
                dictionary: dictionary.clone(),
                recreate: false,
            });
        }
    }
    for (name, dictionary) in &want.dictionaries {
        match have.dictionaries.get(name) {
            None => changes.push(Change::CreateDictionary {
                database: db.clone(),
                dictionary: dictionary.clone(),
                replace: false,
            }),
            Some(old) if old != dictionary => changes.push(Change::CreateDictionary {
                database: db.clone(),
                dictionary: dictionary.clone(),
                replace: true,
            }),
            Some(_) => {}
        }
    }
    Ok(())
}

/// Identity of a grant without its privileges and grant option.
type GrantKey = (
    Grantee,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<String>,
    bool,
);

const ALL: &str = "ALL";

fn grant_key(g: &Grant) -> GrantKey {
    (
        g.grantee.clone(),
        g.database.clone(),
        g.table.clone(),
        g.parameter.clone(),
        g.columns.clone(),
        g.partial_revoke,
    )
}

/// Privileges of one grant key, with and without grant option.
#[derive(Default)]
struct GrantSets {
    template: Option<Grant>,
    with_option: BTreeSet<String>,
    without: BTreeSet<String>,
}

fn index_grants(grants: &[Grant], skip: &BTreeSet<String>) -> BTreeMap<GrantKey, GrantSets> {
    let mut out: BTreeMap<GrantKey, GrantSets> = BTreeMap::new();
    for g in grants {
        if matches!(&g.grantee, Grantee::Role(r) if skip.contains(r)) {
            continue;
        }
        let sets = out.entry(grant_key(g)).or_default();
        sets.template.get_or_insert_with(|| g.clone());
        let target = if g.grant_option {
            &mut sets.with_option
        } else {
            &mut sets.without
        };
        target.extend(g.privileges.iter().cloned());
    }
    out
}

/// Revoke what the current side has and the desired side does not, grant the opposite. Both
/// sides are collapsed privilege sets, so a group replaced by some of its members is revoked
/// and the members granted (revokes run first). A privilege that only loses its grant option
/// gets `REVOKE GRANT OPTION FOR`; one that gains it is granted again `WITH GRANT OPTION`.
fn grants(
    current: &Model,
    desired: &Model,
    recreated_roles: &BTreeSet<String>,
    changes: &mut Vec<Change>,
) {
    let have = index_grants(&current.grants, recreated_roles);
    let want = index_grants(&desired.grants, &BTreeSet::new());
    let empty = GrantSets::default();
    let keys: BTreeSet<&GrantKey> = have.keys().chain(want.keys()).collect();
    for key in keys {
        let old = have.get(key).unwrap_or(&empty);
        let new = want.get(key).unwrap_or(&empty);
        let Some(template) = old.template.as_ref().or(new.template.as_ref()) else {
            continue;
        };
        let diff = |a: &BTreeSet<String>, b: &BTreeSet<String>| -> BTreeSet<String> {
            a.difference(b).cloned().collect()
        };
        let mut downgraded: BTreeSet<String> = diff(&old.with_option, &new.with_option)
            .intersection(&new.without)
            .cloned()
            .collect();
        let mut revoked: BTreeSet<String> = old
            .with_option
            .iter()
            .chain(&old.without)
            .filter(|p| !new.with_option.contains(*p) && !new.without.contains(*p))
            .cloned()
            .collect();
        // `ALL` covers every other privilege: granting it needs no revoke first, only the
        // grant options it does not carry are taken away.
        if new.with_option.contains(ALL) {
            revoked.clear();
            downgraded.clear();
        } else if new.without.contains(ALL) {
            revoked.clear();
            downgraded = diff(&old.with_option, &new.with_option);
        }
        let granted_with = diff(&new.with_option, &old.with_option);
        let granted_without: BTreeSet<String> = new
            .without
            .iter()
            .filter(|p| !old.without.contains(*p) && !old.with_option.contains(*p))
            .cloned()
            .collect();
        let grant = |privileges: BTreeSet<String>, grant_option: bool| Grant {
            privileges,
            grant_option,
            ..template.clone()
        };
        if !revoked.is_empty() {
            changes.push(Change::Revoke(grant(revoked, false)));
        }
        if !downgraded.is_empty() {
            changes.push(Change::RevokeGrantOption(grant(downgraded, true)));
        }
        if !granted_with.is_empty() {
            changes.push(Change::Grant(grant(granted_with, true)));
        }
        if !granted_without.is_empty() {
            changes.push(Change::Grant(grant(granted_without, false)));
        }
    }
}

fn role_grants(
    current: &Model,
    desired: &Model,
    recreated_roles: &BTreeSet<String>,
    changes: &mut Vec<Change>,
) {
    let involves = |g: &RoleGrant| {
        recreated_roles.contains(&g.role)
            || matches!(&g.grantee, Grantee::Role(r) if recreated_roles.contains(r))
    };
    let index = |grants: &[RoleGrant], skip: bool| -> BTreeMap<(Grantee, String), bool> {
        grants
            .iter()
            .filter(|g| !(skip && involves(g)))
            .map(|g| ((g.grantee.clone(), g.role.clone()), g.admin_option))
            .collect()
    };
    let have = index(&current.role_grants, true);
    let want = index(&desired.role_grants, false);
    let grant = |(grantee, role): &(Grantee, String), admin_option: bool| RoleGrant {
        grantee: grantee.clone(),
        role: role.clone(),
        admin_option,
    };
    for (key, admin) in &have {
        match want.get(key) {
            None => changes.push(Change::RevokeRole(grant(key, *admin))),
            Some(false) if *admin => changes.push(Change::RevokeAdminOption(grant(key, true))),
            Some(_) => {}
        }
    }
    for (key, admin) in &want {
        let upgrade = *admin && have.get(key) == Some(&false);
        if !have.contains_key(key) || upgrade {
            changes.push(Change::GrantRole(grant(key, *admin)));
        }
    }
}

fn row_policies(current: &Model, desired: &Model, changes: &mut Vec<Change>) {
    for (id, policy) in &current.row_policies {
        if !desired.row_policies.contains_key(id) {
            changes.push(Change::DropRowPolicy {
                id: id.clone(),
                policy: policy.clone(),
            });
        }
    }
    for (id, policy) in &desired.row_policies {
        let same = |old: &RowPolicy| {
            let mut old = old.clone();
            if policy.storage.is_empty() {
                old.storage.clear();
            }
            &old == policy
        };
        match current.row_policies.get(id) {
            None => changes.push(Change::CreateRowPolicy {
                id: id.clone(),
                policy: policy.clone(),
                replace: false,
            }),
            Some(old) if !same(old) => changes.push(Change::CreateRowPolicy {
                id: id.clone(),
                policy: policy.clone(),
                replace: true,
            }),
            Some(_) => {}
        }
    }
}

/// ClickHouse refuses to drop or rename a table while materialized views or dictionaries depend
/// on it. Dependants that would otherwise stay are dropped and recreated around the change.
fn recreate_blocking_dependants(current: &Model, desired: &Model, changes: &mut Vec<Change>) {
    let mut queue: Vec<ObjectRef> = changes
        .iter()
        .filter_map(|c| match c {
            Change::DropTable {
                database, table, ..
            } => Some(table_ref(database, &table.name)),
            Change::RenameTable { from, .. } => Some(table_ref(&from.database, &from.name)),
            Change::DropMaterializedView { database, view, .. } => Some(ObjectRef::new(
                ObjectKind::MaterializedView,
                [database, &view.name],
            )),
            Change::DropDictionary {
                database,
                dictionary,
                ..
            } => Some(ObjectRef::new(
                ObjectKind::Dictionary,
                [database, &dictionary.name],
            )),
            _ => None,
        })
        .collect();
    let mut seen: BTreeSet<ObjectRef> = queue.iter().cloned().collect();
    let dropped = |changes: &[Change], object: &ObjectRef| {
        changes.iter().any(|c| {
            matches!(
                c,
                Change::DropTable { .. }
                    | Change::DropMaterializedView { .. }
                    | Change::DropDictionary { .. }
                    | Change::DropView { .. }
            ) && &c.object() == object
        })
    };
    while let Some(object) = queue.pop() {
        let dependants: Vec<ObjectRef> = current
            .dependencies
            .iter()
            .filter(|d| !d.soft && d.to == object)
            .map(|d| d.from.clone())
            .collect();
        for dependant in dependants {
            if !seen.insert(dependant.clone()) || dropped(changes, &dependant) {
                continue;
            }
            let [database, name] = dependant.path.as_slice() else {
                continue;
            };
            let added = match dependant.kind {
                ObjectKind::MaterializedView => recreate(
                    current
                        .databases
                        .get(database)
                        .and_then(|d| d.materialized_views.get(name)),
                    desired
                        .databases
                        .get(database)
                        .and_then(|d| d.materialized_views.get(name)),
                    |old| Change::DropMaterializedView {
                        database: database.clone(),
                        view: old.clone(),
                        recreate: true,
                    },
                    |new| Change::CreateMaterializedView {
                        database: database.clone(),
                        view: new.clone(),
                    },
                    changes,
                ),
                ObjectKind::Dictionary => recreate(
                    current
                        .databases
                        .get(database)
                        .and_then(|d| d.dictionaries.get(name)),
                    desired
                        .databases
                        .get(database)
                        .and_then(|d| d.dictionaries.get(name)),
                    |old| Change::DropDictionary {
                        database: database.clone(),
                        dictionary: old.clone(),
                        recreate: true,
                    },
                    |new| Change::CreateDictionary {
                        database: database.clone(),
                        dictionary: new.clone(),
                        replace: false,
                    },
                    changes,
                ),
                _ => false,
            };
            if added {
                queue.push(dependant);
            }
        }
    }
}

/// Drop `old` and create `new` (dropping any pending replace or alter of the same object).
fn recreate<T>(
    old: Option<&T>,
    new: Option<&T>,
    drop: impl Fn(&T) -> Change,
    create: impl Fn(&T) -> Change,
    changes: &mut Vec<Change>,
) -> bool {
    let (Some(old), Some(new)) = (old, new) else {
        return false;
    };
    let drop = drop(old);
    let create = create(new);
    let object = create.object();
    changes.retain(|c| c.object() != object);
    changes.push(drop);
    changes.push(create);
    true
}
