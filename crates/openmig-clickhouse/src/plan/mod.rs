//! Turning [`Change`]s into ordered statements (see `docs/spec/architecture.md`).
//!
//! Every change becomes one step: its statements, a priority (phase, kind rank, object) and the
//! objects it creates, drops or renames. Steps are sorted with the core `DepGraph`:
//!
//! * phase 0 revokes, phase 1 creates and alters, phase 2 drops; within a phase by kind rank;
//! * a drop runs before a create of the same name (recreates, a table replaced by a view);
//! * creating (or renaming into place) an object runs before steps on objects that depend on it
//!   in the desired model, and before steps on objects inside a created database or on a
//!   created role;
//! * dropping an object runs before dropping (or renaming) what it depends on in the current
//!   model, and before dropping its database;
//! * soft dependencies (guessed from SQL text) are dropped when they would form a cycle.
//!
//! ClickHouse has no transactions: migration files run statement by statement and record
//! progress per statement, so statements are idempotent where ClickHouse allows it
//! (`IF [NOT] EXISTS`, `CREATE OR REPLACE`).

pub mod sql;

#[cfg(test)]
mod tests;

use crate::ClickHouseEngine;
use crate::diff::{Change, TableOp};
use crate::model::{Grantee, Model};
use openmig_core::graph::{DepGraph, NodeId, Priority};
use openmig_core::{ChangeInfo, DiffContext, Error, ObjectKind, ObjectRef, Plan, Statement};
use sql::{lit, q, qn};
use std::collections::BTreeSet;

/// How a step affects its object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Create,
    Alter,
    Drop,
    Revoke,
    Grant,
}

struct Step {
    statements: Vec<Statement>,
    effect: Effect,
    object: ObjectRef,
    /// For renames: the old name.
    renamed_from: Option<ObjectRef>,
}

/// The cluster clause for objects whose DDL is replicated by `ON CLUSTER`.
struct Planner<'a> {
    current: &'a Model,
    desired: &'a Model,
    cluster: Option<String>,
}

fn dq(name: &str) -> String {
    format!("\"{name}\"")
}

fn dq2(database: &str, name: &str) -> String {
    format!("\"{database}\".\"{name}\"")
}

/// What a grant is on, for statement comments: `*.*`, `"db".*`, `"db"."t"`.
fn grant_target(g: &crate::model::Grant) -> String {
    match (&g.parameter, &g.database, &g.table) {
        (Some(p), _, _) => dq(p),
        (None, None, _) => "*.*".to_owned(),
        (None, Some(d), None) => format!("{}.*", dq(d)),
        (None, Some(d), Some(t)) => dq2(d, t),
    }
}

fn is_relation(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Dictionary
    )
}

impl Planner<'_> {
    /// ` ON CLUSTER c` when a cluster is configured.
    fn on_cluster(&self) -> String {
        self.cluster
            .as_deref()
            .map_or_else(String::new, |c| format!(" ON CLUSTER {}", q(c)))
    }

    /// The cluster clause for objects in `database`: none inside a `Replicated` database.
    fn db_cluster(&self, database: &str) -> String {
        let replicated = self
            .desired
            .databases
            .get(database)
            .or_else(|| self.current.databases.get(database))
            .is_some_and(|d| d.is_replicated());
        if replicated {
            String::new()
        } else {
            self.on_cluster()
        }
    }

    /// The cluster clause for access entities: none for replicated user directories.
    fn storage_cluster(&self, storage: &str) -> String {
        if storage == "replicated" {
            String::new()
        } else {
            self.on_cluster()
        }
    }

    fn grantee_cluster(&self, grantee: &Grantee) -> String {
        let storage = match grantee {
            Grantee::Role(name) => self
                .desired
                .roles
                .get(name)
                .or_else(|| self.current.roles.get(name))
                .map(|r| r.storage.as_str()),
            Grantee::User(name) => self.current.users.get(name).map(|u| u.storage.as_str()),
        };
        self.storage_cluster(storage.unwrap_or_default())
    }

    fn step(&self, change: &Change) -> Step {
        let object = change.object();
        let destructive = change.destructive();
        let stmt = |sql: String, comment: String| {
            let mut s = Statement::new(sql, comment).origin(object.clone());
            if let Some(reason) = &destructive {
                s = s.destructive(reason.clone());
            }
            s
        };
        let mut renamed_from = None;
        let (effect, statements) = match change {
            Change::CreateDatabase(db) => {
                let mut sql = format!(
                    "CREATE DATABASE IF NOT EXISTS {}{} ENGINE = {}",
                    q(&db.name),
                    self.on_cluster(),
                    db.engine
                );
                if let Some(comment) = &db.comment {
                    sql.push_str(&format!(" COMMENT {}", lit(comment)));
                }
                (
                    Effect::Create,
                    vec![stmt(sql, format!("Create database {}", dq(&db.name)))],
                )
            }
            Change::DropDatabase(db) => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP DATABASE IF EXISTS {}{} SYNC",
                        q(&db.name),
                        self.on_cluster()
                    ),
                    format!("Drop database {}", dq(&db.name)),
                )],
            ),
            Change::CommentDatabase { name, comment } => (
                Effect::Alter,
                vec![stmt(
                    format!(
                        "ALTER DATABASE {}{} MODIFY COMMENT {}",
                        q(name),
                        self.on_cluster(),
                        lit(comment.as_deref().unwrap_or_default())
                    ),
                    format!("Comment on database {}", dq(name)),
                )],
            ),
            Change::CreateTable { database, table } => (
                Effect::Create,
                vec![stmt(
                    sql::create_table(database, table, &self.db_cluster(database)),
                    format!("Create table {}", dq2(database, &table.name)),
                )],
            ),
            Change::DropTable {
                database, table, ..
            } => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP TABLE IF EXISTS {}{} SYNC",
                        qn(database, &table.name),
                        self.db_cluster(database)
                    ),
                    format!("Drop table {}", dq2(database, &table.name)),
                )],
            ),
            Change::RenameTable { from, to } => {
                renamed_from = Some(ObjectRef::new(
                    ObjectKind::Table,
                    [&from.database, &from.name],
                ));
                (
                    Effect::Create,
                    vec![stmt(
                        format!(
                            "RENAME TABLE {} TO {}{}",
                            qn(&from.database, &from.name),
                            qn(&to.database, &to.name),
                            self.db_cluster(&from.database)
                        ),
                        format!(
                            "Rename table {} to {}",
                            dq2(&from.database, &from.name),
                            dq2(&to.database, &to.name)
                        ),
                    )],
                )
            }
            Change::AlterTable {
                database,
                table,
                ops,
            } => (Effect::Alter, self.alter_table(database, table, ops, &stmt)),
            Change::DropColumns {
                database,
                table,
                columns,
            } => {
                let ops: Vec<TableOp> = columns.iter().cloned().map(TableOp::DropColumn).collect();
                (
                    Effect::Alter,
                    self.alter_table(database, table, &ops, &stmt),
                )
            }
            Change::CreateView {
                database,
                view,
                replace,
            } => (
                Effect::Create,
                vec![stmt(
                    sql::create_view(database, view, &self.db_cluster(database)),
                    format!(
                        "{} view {}",
                        if *replace { "Replace" } else { "Create" },
                        dq2(database, &view.name)
                    ),
                )],
            ),
            Change::DropView { database, view, .. } => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP VIEW IF EXISTS {}{} SYNC",
                        qn(database, &view.name),
                        self.db_cluster(database)
                    ),
                    format!("Drop view {}", dq2(database, &view.name)),
                )],
            ),
            Change::CreateMaterializedView { database, view } => (
                Effect::Create,
                vec![stmt(
                    sql::create_materialized_view(database, view, &self.db_cluster(database)),
                    format!("Create materialized view {}", dq2(database, &view.name)),
                )],
            ),
            Change::DropMaterializedView { database, view, .. } => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP VIEW IF EXISTS {}{} SYNC",
                        qn(database, &view.name),
                        self.db_cluster(database)
                    ),
                    format!("Drop materialized view {}", dq2(database, &view.name)),
                )],
            ),
            Change::ModifyQuery {
                database,
                view,
                query,
            } => (
                Effect::Alter,
                vec![stmt(
                    format!(
                        "ALTER TABLE {}{} MODIFY QUERY\n{query}",
                        qn(database, view),
                        self.db_cluster(database)
                    ),
                    format!("Modify query of materialized view {}", dq2(database, view)),
                )],
            ),
            Change::CommentMaterializedView {
                database,
                view,
                comment,
            } => (
                Effect::Alter,
                vec![stmt(
                    format!(
                        "ALTER TABLE {}{} MODIFY COMMENT {}",
                        qn(database, view),
                        self.db_cluster(database),
                        lit(comment.as_deref().unwrap_or_default())
                    ),
                    format!("Comment on materialized view {}", dq2(database, view)),
                )],
            ),
            Change::CreateDictionary {
                database,
                dictionary,
                replace,
            } => (
                Effect::Create,
                vec![stmt(
                    sql::create_dictionary(database, dictionary, &self.db_cluster(database)),
                    format!(
                        "{} dictionary {}",
                        if *replace { "Replace" } else { "Create" },
                        dq2(database, &dictionary.name)
                    ),
                )],
            ),
            Change::DropDictionary {
                database,
                dictionary,
                ..
            } => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP DICTIONARY IF EXISTS {}{} SYNC",
                        qn(database, &dictionary.name),
                        self.db_cluster(database)
                    ),
                    format!("Drop dictionary {}", dq2(database, &dictionary.name)),
                )],
            ),
            Change::CreateFunction { function, replace } => (
                Effect::Create,
                vec![stmt(
                    sql::create_function(function, &self.on_cluster()),
                    format!(
                        "{} function {}",
                        if *replace { "Replace" } else { "Create" },
                        dq(&function.name)
                    ),
                )],
            ),
            Change::DropFunction(function) => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP FUNCTION IF EXISTS {}{}",
                        q(&function.name),
                        self.on_cluster()
                    ),
                    format!("Drop function {}", dq(&function.name)),
                )],
            ),
            Change::CreateRole(role) => {
                let mut sql = format!(
                    "CREATE ROLE IF NOT EXISTS {}{}",
                    q(&role.name),
                    self.storage_cluster(&role.storage)
                );
                if !role.storage.is_empty() {
                    sql.push_str(&format!(" IN {}", q(&role.storage)));
                }
                (
                    Effect::Create,
                    vec![stmt(sql, format!("Create role {}", dq(&role.name)))],
                )
            }
            Change::DropRole { role, .. } => (
                Effect::Drop,
                vec![stmt(
                    format!(
                        "DROP ROLE IF EXISTS {}{}",
                        q(&role.name),
                        self.storage_cluster(&role.storage)
                    ),
                    format!("Drop role {}", dq(&role.name)),
                )],
            ),
            Change::Grant(g) => (
                Effect::Grant,
                vec![stmt(
                    sql::grant(g, &self.grantee_cluster(&g.grantee)),
                    format!("Grant on {} to {}", grant_target(g), dq(g.grantee.name())),
                )],
            ),
            Change::Revoke(g) => (
                Effect::Revoke,
                vec![stmt(
                    sql::revoke(g, &self.grantee_cluster(&g.grantee)),
                    format!(
                        "Revoke on {} from {}",
                        grant_target(g),
                        dq(g.grantee.name())
                    ),
                )],
            ),
            Change::RevokeGrantOption(g) => (
                Effect::Revoke,
                vec![stmt(
                    sql::revoke_grant_option(g, &self.grantee_cluster(&g.grantee)),
                    format!(
                        "Revoke grant option on {} from {}",
                        grant_target(g),
                        dq(g.grantee.name())
                    ),
                )],
            ),
            Change::RevokeAdminOption(g) => (
                Effect::Revoke,
                vec![stmt(
                    format!(
                        "REVOKE{} ADMIN OPTION FOR {} FROM {}",
                        self.grantee_cluster(&g.grantee),
                        q(&g.role),
                        q(g.grantee.name())
                    ),
                    format!(
                        "Revoke admin option on role {} from {}",
                        dq(&g.role),
                        dq(g.grantee.name())
                    ),
                )],
            ),
            Change::GrantRole(g) => (
                Effect::Grant,
                vec![stmt(
                    sql::grant_role(g, &self.grantee_cluster(&g.grantee)),
                    format!("Grant role {} to {}", dq(&g.role), dq(g.grantee.name())),
                )],
            ),
            Change::RevokeRole(g) => (
                Effect::Revoke,
                vec![stmt(
                    sql::revoke_role(g, &self.grantee_cluster(&g.grantee)),
                    format!("Revoke role {} from {}", dq(&g.role), dq(g.grantee.name())),
                )],
            ),
            Change::CreateRowPolicy {
                id,
                policy,
                replace,
            } => (
                Effect::Create,
                vec![stmt(
                    sql::create_row_policy(id, policy, &self.storage_cluster(&policy.storage)),
                    format!(
                        "{} row policy {} on {}",
                        if *replace { "Replace" } else { "Create" },
                        dq(&id.name),
                        dq2(&id.database, &id.table)
                    ),
                )],
            ),
            Change::DropRowPolicy { id, policy } => (
                Effect::Drop,
                vec![stmt(
                    sql::drop_row_policy(id, &self.storage_cluster(&policy.storage)),
                    format!(
                        "Drop row policy {} on {}",
                        dq(&id.name),
                        dq2(&id.database, &id.table)
                    ),
                )],
            ),
        };
        Step {
            statements,
            effect,
            object,
            renamed_from,
        }
    }

    /// One statement per command, except that columns appended to the sorting key are added in
    /// the same statement as the `MODIFY ORDER BY` (ClickHouse requires it).
    fn alter_table(
        &self,
        database: &str,
        table: &str,
        ops: &[TableOp],
        stmt: &dyn Fn(String, String) -> Statement,
    ) -> Vec<Statement> {
        let prefix = format!(
            "ALTER TABLE {}{}",
            qn(database, table),
            self.db_cluster(database)
        );
        let target = dq2(database, table);
        let column_origin =
            |column: &str| ObjectRef::new(ObjectKind::Column, [database, table, column]);
        let appended: Vec<&TableOp> = if ops.iter().any(|o| matches!(o, TableOp::ModifyOrderBy(_)))
        {
            ops.iter()
                .filter(|o| matches!(o, TableOp::AddColumn { .. } | TableOp::ModifyOrderBy(_)))
                .collect()
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        if !appended.is_empty() {
            let commands: Vec<String> = appended.iter().map(|o| sql::table_op(o)).collect();
            out.push(stmt(
                format!("{prefix}\n    {}", commands.join(",\n    ")),
                format!("Add columns to the sorting key of {target}"),
            ));
        }
        for op in ops {
            if appended.iter().any(|a| std::ptr::eq(*a, op)) {
                continue;
            }
            let (comment, origin) = match op {
                TableOp::AddColumn { column, .. } => (
                    format!("Add column {} to {target}", dq(&column.name)),
                    Some(column_origin(&column.name)),
                ),
                TableOp::DropColumn(column) => (
                    format!("Drop column {} from {target}", dq(&column.name)),
                    None,
                ),
                TableOp::RenameColumn { from, to } => (
                    format!("Rename column {} to {} in {target}", dq(from), dq(to)),
                    Some(column_origin(to)),
                ),
                TableOp::ModifyColumn { column, .. } => (
                    format!("Modify column {} of {target}", dq(column)),
                    Some(column_origin(column)),
                ),
                TableOp::AddIndex { name, .. } => (
                    format!("Add index {} to {target}", dq(name)),
                    Some(ObjectRef::new(ObjectKind::Index, [database, table, name])),
                ),
                TableOp::DropIndex(name) => {
                    (format!("Drop index {} from {target}", dq(name)), None)
                }
                TableOp::AddProjection { name, .. } => (
                    format!("Add projection {} to {target}", dq(name)),
                    Some(ObjectRef::new(
                        ObjectKind::Projection,
                        [database, table, name],
                    )),
                ),
                TableOp::DropProjection(name) => {
                    (format!("Drop projection {} from {target}", dq(name)), None)
                }
                TableOp::AddConstraint { name, .. } => (
                    format!("Add constraint {} to {target}", dq(name)),
                    Some(ObjectRef::new(
                        ObjectKind::Constraint,
                        [database, table, name],
                    )),
                ),
                TableOp::DropConstraint(name) => {
                    (format!("Drop constraint {} from {target}", dq(name)), None)
                }
                TableOp::ModifyOrderBy(_) => (format!("Modify the sorting key of {target}"), None),
                TableOp::ModifySampleBy(_) => (format!("Modify SAMPLE BY of {target}"), None),
                TableOp::RemoveSampleBy => (format!("Remove SAMPLE BY from {target}"), None),
                TableOp::ModifyTtl(_) => (format!("Modify TTL of {target}"), None),
                TableOp::RemoveTtl => (format!("Remove TTL from {target}"), None),
                TableOp::ModifySettings(_) => (format!("Modify settings of {target}"), None),
                TableOp::ResetSettings(_) => (format!("Reset settings of {target}"), None),
                TableOp::ModifyComment(_) => (format!("Comment on table {target}"), None),
            };
            let mut s = stmt(format!("{prefix} {}", sql::table_op(op)), comment);
            if let Some(origin) = origin {
                s.origin = Some(origin);
            }
            out.push(s);
        }
        out
    }
}

/// Phase and kind rank of a change.
fn priority(change: &Change) -> (u8, u16) {
    match change {
        Change::Revoke(_) | Change::RevokeGrantOption(_) => (0, 0),
        Change::RevokeRole(_) | Change::RevokeAdminOption(_) => (0, 1),
        Change::CreateRole(_) => (1, 0),
        Change::CreateDatabase(_) | Change::CommentDatabase { .. } => (1, 1),
        Change::CreateFunction { .. } => (1, 2),
        Change::CreateTable { .. } | Change::RenameTable { .. } => (1, 3),
        Change::AlterTable { .. } => (1, 4),
        Change::CreateDictionary { .. } => (1, 5),
        Change::CreateView { .. } => (1, 6),
        Change::CreateMaterializedView { .. }
        | Change::ModifyQuery { .. }
        | Change::CommentMaterializedView { .. } => (1, 7),
        Change::CreateRowPolicy { .. } => (1, 8),
        Change::Grant(g) if g.partial_revoke => (1, 11),
        Change::Grant(_) => (1, 9),
        Change::GrantRole(_) => (1, 10),
        Change::DropRowPolicy { .. } => (2, 0),
        Change::DropMaterializedView { .. } => (2, 1),
        Change::DropView { .. } => (2, 2),
        Change::DropDictionary { .. } => (2, 3),
        Change::DropColumns { .. } => (2, 4),
        Change::DropTable { .. } => (2, 5),
        Change::DropFunction(_) => (2, 6),
        Change::DropDatabase(_) => (2, 7),
        Change::DropRole { .. } => (2, 8),
    }
}

/// Roles a change grants to or mentions.
fn mentioned_roles(change: &Change) -> Vec<String> {
    let role = |g: &Grantee| match g {
        Grantee::Role(r) => vec![r.clone()],
        Grantee::User(_) => Vec::new(),
    };
    match change {
        Change::Grant(g) => role(&g.grantee),
        Change::GrantRole(g) => {
            let mut out = role(&g.grantee);
            out.push(g.role.clone());
            out
        }
        Change::CreateRowPolicy { policy, .. } => policy
            .to
            .names
            .iter()
            .chain(&policy.to.except)
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// What the ordering needs to know about a step.
struct Node {
    id: NodeId,
    effect: Effect,
    object: ObjectRef,
    /// The old name of a renamed table.
    renamed_from: Option<ObjectRef>,
    /// Roles the step grants to or names.
    roles: Vec<String>,
    /// A late `DROP COLUMN` step.
    column_drop: bool,
}

/// Order `changes` and render them.
pub fn plan(
    changes: Vec<Change>,
    current: &Model,
    desired: &Model,
    ctx: &DiffContext<'_, ClickHouseEngine>,
) -> Result<Plan, Error> {
    let planner = Planner {
        current,
        desired,
        cluster: ctx.settings.cluster.clone(),
    };
    let mut graph: DepGraph<Step> = DepGraph::new();
    let mut nodes: Vec<Node> = Vec::new();
    for change in &changes {
        let (phase, rank) = priority(change);
        let step = planner.step(change);
        let (effect, object, renamed_from) =
            (step.effect, step.object.clone(), step.renamed_from.clone());
        let id = graph.add(step, Priority::new(phase, rank, object.dotted()));
        nodes.push(Node {
            id,
            effect,
            object,
            renamed_from,
            roles: mentioned_roles(change),
            column_drop: matches!(change, Change::DropColumns { .. }),
        });
    }
    for (before, after, soft) in edges(&nodes, current, desired) {
        if soft {
            graph.soft_edge(before, after);
        } else {
            graph.edge(before, after);
        }
    }
    let steps = graph.sort().map_err(|cycle| {
        let objects: Vec<String> = cycle.items.iter().map(|s| s.object.to_string()).collect();
        Error::other(format!(
            "cannot order the plan: circular dependencies between {}",
            objects.join(", ")
        ))
    })?;
    Ok(Plan {
        statements: steps.into_iter().flat_map(|s| s.statements).collect(),
        warnings: Vec::new(),
    })
}

/// Ordering edges `(before, after, soft)`, hard edges first, without duplicates.
fn edges(nodes: &[Node], current: &Model, desired: &Model) -> Vec<(NodeId, NodeId, bool)> {
    let mut out: Vec<(NodeId, NodeId, bool)> = Vec::new();
    let of = |effect: Effect| nodes.iter().filter(move |n| n.effect == effect);
    let same_name = |a: &ObjectRef, b: &ObjectRef| {
        a.path == b.path && (a.kind == b.kind || (is_relation(a.kind) && is_relation(b.kind)))
    };

    for drop in of(Effect::Drop) {
        for create in of(Effect::Create) {
            if same_name(&drop.object, &create.object) {
                out.push((drop.id, create.id, false));
            }
        }
    }
    for dep in &desired.dependencies {
        for provider in of(Effect::Create).filter(|n| n.object == dep.to) {
            for user in nodes.iter().filter(|n| {
                matches!(n.effect, Effect::Create | Effect::Alter) && n.object == dep.from
            }) {
                out.push((provider.id, user.id, dep.soft));
            }
        }
    }
    for create in of(Effect::Create) {
        let name = create.object.path.first();
        for n in nodes {
            let inside_database = create.object.kind == ObjectKind::Database
                && !matches!(n.object.kind, ObjectKind::Database | ObjectKind::Grant)
                && n.object.path.len() > 1
                && n.object.path.first() == name
                && matches!(n.effect, Effect::Create | Effect::Alter);
            let uses_role =
                create.object.kind == ObjectKind::Role && n.roles.iter().any(|r| Some(r) == name);
            if inside_database || uses_role {
                out.push((create.id, n.id, false));
            }
        }
    }
    for dep in &current.dependencies {
        for user in of(Effect::Drop).filter(|n| n.object == dep.from) {
            for drop in of(Effect::Drop).filter(|n| n.object == dep.to) {
                out.push((user.id, drop.id, dep.soft));
            }
            for rename in nodes
                .iter()
                .filter(|n| n.renamed_from.as_ref() == Some(&dep.to))
            {
                out.push((user.id, rename.id, false));
            }
        }
        for column_drop in nodes.iter().filter(|n| n.column_drop && n.object == dep.to) {
            for user in nodes.iter().filter(|n| n.object == dep.from) {
                out.push((user.id, column_drop.id, false));
            }
        }
    }
    for database in of(Effect::Drop).filter(|n| n.object.kind == ObjectKind::Database) {
        for inner in of(Effect::Drop).filter(|n| {
            n.object.kind != ObjectKind::Database
                && n.object.path.len() > 1
                && n.object.path.first() == database.object.path.first()
        }) {
            out.push((inner.id, database.id, false));
        }
    }

    out.sort_by_key(|(_, _, soft)| *soft);
    let mut seen: BTreeSet<(NodeId, NodeId)> = BTreeSet::new();
    out.retain(|(before, after, _)| seen.insert((*before, *after)));
    out
}
