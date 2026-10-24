//! Turning [`Change`]s into an ordered [`Plan`].
//!
//! 1. The current model is re-keyed with the renames, exactly as the diff saw it.
//! 2. [`rebuild::expand`] finds dependants that block a change (a view on a column whose type
//!    changes, a default calling a function that is recreated, ...) and schedules them to be
//!    dropped before and created after it; changes they supersede are removed.
//! 3. Every change becomes [`Step`]s: statements plus the objects they create, remove, release and
//!    require ([`create`], [`alter`], [`data`]).
//! 4. [`order`] connects the steps in a [`openmig_core::graph::DepGraph`] and sorts them by
//!    (phase, rank, key).
//! 5. Transaction requirements are settled: `CONCURRENTLY` and enum values used after being added
//!    make statements `Forbidden`; plans creating SQL functions start with
//!    `SET check_function_bodies = off`.

mod alter;
mod create;
mod data;
mod order;
pub(crate) mod rebuild;
pub(crate) mod refs;
mod step;
#[cfg(test)]
mod tests;

use crate::PostgresEngine;
use crate::diff::{AclDelta, Change, Options, rename};
use crate::model::{Database, FunctionBody, Object, Privilege, QualifiedName};
use crate::sql;
use openmig_core::{DiffContext, Error, ObjectKind, ObjectRef, Plan, PlanMode, Statement};
use rebuild::Rebuild;
use std::collections::{BTreeMap, BTreeSet};
use step::{Phase, Step, rank};

/// Plan `changes` (from [`crate::diff::diff`] with the same arguments).
pub fn plan(
    changes: Vec<Change>,
    current: &Database,
    desired: &Database,
    ctx: &DiffContext<'_, PostgresEngine>,
) -> Result<Plan, Error> {
    let opts = Options::new(ctx, current, desired);
    let renames = rename::applicable(current, ctx.renames);
    let rekeyed = rename::rekey(current, &renames);
    let mut planner = Planner::new(&rekeyed, desired, &opts, &changes);
    let rebuilds = rebuild::expand(&planner, &changes)?;
    let changes = rebuild::supersede(changes, &rebuilds);
    planner.mark_rebuilds(&rebuilds);
    planner.external_roles();
    for change in &changes {
        planner.change(change)?;
    }
    for (object, how) in &rebuilds {
        planner.rebuild(object, *how)?;
    }
    planner.restore_replica_identities(&changes);
    let steps = order::sort(planner.steps, &rekeyed, desired, &opts)?;
    Ok(finish(steps, desired))
}

/// State shared by the step builders.
pub(crate) struct Planner<'a> {
    /// The current model with renames applied.
    pub cur: &'a Database,
    pub des: &'a Database,
    pub opts: &'a Options<'a>,
    pub steps: Vec<Step>,
    /// Current objects the plan drops or recreates, with everything nested in them.
    pub removed: BTreeSet<ObjectRef>,
    /// Tables that do not exist before the plan (created, or dropped and created again).
    pub new_tables: BTreeSet<QualifiedName>,
    /// Foreign keys written inside `CREATE TABLE`.
    pub inline_fks: BTreeSet<ObjectRef>,
    /// Foreign keys of dropped tables that are dropped on their own first (they form a cycle).
    pub split_fks: BTreeSet<ObjectRef>,
    /// Columns and domains whose default is dropped and set again around a blocking change.
    pub default_rebuilt: BTreeSet<ObjectRef>,
    /// The desired model's dependencies, by dependent object.
    pub des_dependencies: BTreeMap<&'a ObjectRef, Vec<&'a ObjectRef>>,
}

impl<'a> Planner<'a> {
    fn new(
        cur: &'a Database,
        des: &'a Database,
        opts: &'a Options<'a>,
        changes: &[Change],
    ) -> Self {
        let mut p = Planner {
            cur,
            des,
            opts,
            steps: Vec::new(),
            removed: BTreeSet::new(),
            new_tables: BTreeSet::new(),
            inline_fks: BTreeSet::new(),
            split_fks: BTreeSet::new(),
            default_rebuilt: BTreeSet::new(),
            des_dependencies: BTreeMap::new(),
        };
        for d in &des.dependencies {
            p.des_dependencies.entry(&d.from).or_default().push(&d.to);
        }
        let mut dropped_tables = BTreeSet::new();
        for c in changes {
            match c {
                Change::Drop(r) | Change::Recreate { object: r, .. } => {
                    p.mark_removed(r);
                    if r.kind == ObjectKind::Table {
                        let name = sql::relation_of(r);
                        if matches!(c, Change::Recreate { .. }) {
                            p.new_tables.insert(name.clone());
                        }
                        dropped_tables.insert(name);
                    }
                }
                Change::Create(r) if r.kind == ObjectKind::Table => {
                    p.new_tables.insert(sql::relation_of(r));
                }
                _ => {}
            }
        }
        p.inline_fks = create::inline_foreign_keys(des, &p.new_tables, &p.removed);
        p.split_fks = create::cyclic_foreign_keys(cur, &dropped_tables);
        p
    }

    /// Record that `r` (and everything nested in it) is removed.
    fn mark_removed(&mut self, r: &ObjectRef) {
        self.removed.insert(r.clone());
        self.removed.extend(nested(self.cur, r));
    }

    fn mark_rebuilds(&mut self, rebuilds: &BTreeMap<ObjectRef, Rebuild>) {
        for (r, how) in rebuilds {
            if how.removes_object() {
                self.mark_removed(r);
            } else {
                self.default_rebuilt.insert(r.clone());
            }
        }
    }

    pub fn scratch(&self) -> bool {
        self.opts.mode == PlanMode::Scratch
    }

    pub fn push(&mut self, step: Step) {
        self.steps.push(step);
    }

    /// Clear the destructive marks of the steps from `first` on: what they drop is created
    /// again without losing data.
    pub fn keep_data(&mut self, first: usize) {
        for step in &mut self.steps[first..] {
            for st in &mut step.statements {
                st.destructive = None;
            }
        }
    }

    fn change(&mut self, change: &Change) -> Result<(), Error> {
        match change {
            Change::Create(r) => self.create(r),
            Change::Drop(r) => self.drop(r),
            Change::Recreate {
                object,
                destructive,
                ..
            } => {
                let first = self.steps.len();
                self.drop(object)?;
                if !destructive {
                    self.keep_data(first);
                }
                self.create(object)
            }
            Change::Alter { object, ops } => self.alter(object, ops),
            Change::Rename { from, to } => {
                self.rename(from, to);
                Ok(())
            }
            Change::Data { table, rows } => {
                self.data(table, rows);
                Ok(())
            }
        }
    }

    /// A table whose replica identity index is dropped falls back to no replica identity; set it
    /// again when the plan recreates that index (or constraint) on a table that stays.
    fn restore_replica_identities(&mut self, changes: &[Change]) {
        let altered: BTreeSet<ObjectRef> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Alter { object, ops }
                    if ops.contains(&crate::diff::AlterOp::ReplicaIdentity) =>
                {
                    Some(object.clone())
                }
                _ => None,
            })
            .collect();
        let mut restore = Vec::new();
        for schema in self.des.schemas.values() {
            for t in schema.tables.values() {
                let crate::model::ReplicaIdentity::Index(index) = &t.replica_identity else {
                    continue;
                };
                let table = ObjectRef::new(ObjectKind::Table, [&schema.name, &t.name]);
                let lost = [
                    ObjectKind::Index,
                    ObjectKind::PrimaryKey,
                    ObjectKind::Unique,
                ]
                .into_iter()
                .any(|k| {
                    self.removed
                        .contains(&ObjectRef::new(k, [&schema.name, &t.name, index]))
                });
                if lost && !self.removed.contains(&table) && !altered.contains(&table) {
                    restore.push((table, t.replica_identity.clone()));
                }
            }
        }
        for (table, identity) in restore {
            self.replica_identity_step(&table, &identity);
        }
    }

    /// Roles that a decoded schema names without declaring them: created `NOLOGIN` in scratch
    /// databases when missing.
    fn external_roles(&mut self) {
        if !self.scratch() {
            return;
        }
        for name in &self.des.external_roles {
            if self.des.roles.contains_key(name) || self.cur.roles.contains_key(name) {
                continue;
            }
            let r = ObjectRef::new(ObjectKind::Role, [name]);
            let mut step = Step::new(&r, Phase::Create, rank::ROLE, "");
            step.sql(
                sql::create_role_if_missing(name),
                format!("Create role {} if missing", sql::ident(name)),
            );
            step.creates.push(r);
            self.push(step);
        }
    }

    /// `COMMENT ON` step for `r` (looked up in the desired model).
    pub fn comment_step(&mut self, r: &ObjectRef, comment: Option<&str>) {
        let Some(target) = sql::object_target(r, self.des) else {
            return;
        };
        let mut step = Step::new(r, Phase::Create, rank::COMMENT, " comment");
        step.sql(
            sql::comment_on(&target, comment),
            format!("Set comment on {}", sql::noun(r)),
        );
        step.requires.push(r.clone());
        self.push(step);
    }

    /// `GRANT` / `REVOKE` step for `r` (a column grants on its relation).
    pub fn acl_step(&mut self, r: &ObjectRef, delta: &AclDelta) {
        let (target, column) = match grant_target(r, self.des) {
            Some(t) => t,
            None => return,
        };
        let mut step = Step::new(r, Phase::Create, rank::GRANT, " grant");
        step.requires.push(r.clone());
        let noun = sql::noun(r);
        let column = column.as_deref();
        for (grantee, privileges) in by_grantee(delta.revoke.iter()) {
            step.sql(
                sql::revoke(&target, column, &privileges, &grantee),
                format!("Revoke on {noun} from {}", sql::role_spec(&grantee)),
            );
            step.revokes_from.push(grantee);
        }
        for (grantee, privileges) in by_grantee(delta.revoke_grant_option.iter()) {
            step.sql(
                sql::revoke_grant_option(&target, column, &privileges, &grantee),
                format!(
                    "Revoke grant option on {noun} from {}",
                    sql::role_spec(&grantee)
                ),
            );
        }
        for grantable in [false, true] {
            let items = delta.grant.iter().filter(|i| i.grantable == grantable);
            for (grantee, privileges) in by_grantee(items) {
                step.sql(
                    sql::grant(&target, column, &privileges, &grantee, grantable),
                    format!("Grant on {noun} to {}", sql::role_spec(&grantee)),
                );
                if grantee != crate::model::PUBLIC {
                    step.requires
                        .push(ObjectRef::new(ObjectKind::Role, [&grantee]));
                }
            }
        }
        if !step.statements.is_empty() {
            self.push(step);
        }
    }
}

/// Privileges grouped by grantee, in a stable order.
fn by_grantee<'i>(
    items: impl Iterator<Item = &'i crate::model::AclItem>,
) -> BTreeMap<String, Vec<Privilege>> {
    let mut out: BTreeMap<String, Vec<Privilege>> = BTreeMap::new();
    for i in items {
        out.entry(i.grantee.clone()).or_default().push(i.privilege);
    }
    for privileges in out.values_mut() {
        privileges.sort();
        privileges.dedup();
    }
    out
}

/// What follows `ON` in `GRANT` for `r`, and the column for column privileges.
fn grant_target(r: &ObjectRef, db: &Database) -> Option<(String, Option<String>)> {
    use ObjectKind as K;
    let p = &r.path;
    Some(match (r.kind, p.as_slice()) {
        (K::Schema, [s]) => (format!("SCHEMA {}", sql::ident(s)), None),
        (K::Table | K::View | K::MaterializedView, [s, n]) => {
            (format!("TABLE {}", sql::qualified(s, n)), None)
        }
        (K::Column, [s, t, c]) => (format!("TABLE {}", sql::qualified(s, t)), Some(c.clone())),
        (K::Sequence, [s, n]) => (format!("SEQUENCE {}", sql::qualified(s, n)), None),
        (K::Enum | K::Composite, [s, n]) => (format!("TYPE {}", sql::qualified(s, n)), None),
        (K::Domain, [s, n]) => (format!("DOMAIN {}", sql::qualified(s, n)), None),
        (K::Function, [s, _]) => match db.get(r)? {
            Object::Function(f) => (format!("FUNCTION {}", sql::routine_name(s, f)), None),
            _ => return None,
        },
        (K::Procedure, [s, _]) => match db.get(r)? {
            Object::Procedure(f) => (format!("PROCEDURE {}", sql::routine_name(s, f)), None),
            _ => return None,
        },
        _ => return None,
    })
}

/// Objects nested in `r` in `db`: columns, constraints, indexes, policies and triggers of a
/// relation, checks of a domain, fields of a composite type.
pub(crate) fn nested(db: &Database, r: &ObjectRef) -> Vec<ObjectRef> {
    if !matches!(
        r.kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Composite
            | ObjectKind::Domain
    ) {
        return Vec::new();
    }
    let Some(schema) = db.schemas.get(&r.path[0]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut objects = Vec::new();
    schema_children(schema, &r.path[1], &mut objects);
    for o in objects {
        if is_nested_in(&o, r) {
            out.push(o);
        }
    }
    out
}

fn schema_children(schema: &crate::model::Schema, rel: &str, out: &mut Vec<ObjectRef>) {
    let s = &schema.name;
    let r3 = |k: ObjectKind, n: &str| ObjectRef::new(k, [s.as_str(), rel, n]);
    if let Some(t) = schema.tables.get(rel) {
        out.extend(t.columns.iter().map(|c| r3(ObjectKind::Column, &c.name)));
        if let Some(pk) = &t.primary_key {
            out.push(r3(ObjectKind::PrimaryKey, &pk.name));
        }
        out.extend(t.foreign_keys.keys().map(|n| r3(ObjectKind::ForeignKey, n)));
        out.extend(t.uniques.keys().map(|n| r3(ObjectKind::Unique, n)));
        out.extend(t.checks.keys().map(|n| r3(ObjectKind::Check, n)));
        out.extend(t.exclusions.keys().map(|n| r3(ObjectKind::Exclusion, n)));
        out.extend(
            t.standalone_indexes()
                .map(|i| r3(ObjectKind::Index, &i.name)),
        );
        out.extend(t.policies.keys().map(|n| r3(ObjectKind::Policy, n)));
        out.extend(t.triggers.keys().map(|n| r3(ObjectKind::Trigger, n)));
    }
    if let Some(v) = schema.views.get(rel) {
        out.extend(v.columns.iter().map(|c| r3(ObjectKind::Column, &c.name)));
        out.extend(v.triggers.keys().map(|n| r3(ObjectKind::Trigger, n)));
    }
    if let Some(m) = schema.materialized_views.get(rel) {
        out.extend(m.columns.iter().map(|c| r3(ObjectKind::Column, &c.name)));
        out.extend(m.indexes.keys().map(|n| r3(ObjectKind::Index, n)));
    }
    if let Some(c) = schema.composites.get(rel) {
        out.extend(c.fields.iter().map(|f| r3(ObjectKind::Column, &f.name)));
    }
    if let Some(d) = schema.domains.get(rel) {
        out.extend(d.checks.keys().map(|n| r3(ObjectKind::Check, n)));
    }
}

/// Whether `inner` lives inside `outer` (a relation, composite type or domain).
pub(crate) fn is_nested_in(inner: &ObjectRef, outer: &ObjectRef) -> bool {
    matches!(
        outer.kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Composite
            | ObjectKind::Domain
    ) && inner.path.len() == outer.path.len() + 1
        && inner.path.starts_with(&outer.path)
}

/// Flatten sorted steps into the plan and settle transaction requirements.
fn finish(steps: Vec<Step>, desired: &Database) -> Plan {
    let mut steps = steps;
    order::forbid_enum_values_in_use(&mut steps, desired);
    let creates_sql_functions = steps.iter().any(|s| {
        matches!(s.subject.kind, ObjectKind::Function | ObjectKind::Procedure)
            && s.statements.iter().any(|st| st.sql.starts_with("CREATE "))
            && match desired.get(&s.subject) {
                Some(Object::Function(f) | Object::Procedure(f)) => {
                    f.language == "sql" && matches!(f.body, FunctionBody::Definition(_))
                }
                _ => false,
            }
    });
    let mut plan = Plan::default();
    if creates_sql_functions {
        plan.push(Statement::new(
            "SET check_function_bodies = off",
            "Let SQL function bodies reference objects created later",
        ));
    }
    for step in steps {
        for st in step.statements {
            plan.push(st);
        }
    }
    plan
}
