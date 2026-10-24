//! Steps that create, drop and rename whole objects.

use super::refs::{functions_named, referenced_key, type_refs};
use super::step::{Phase, Step, StepKind, drop_rank, rank};
use super::{Planner, nested};
use crate::diff::{acl_for_new, drop_destroys, inherited_columns};
use crate::model::{
    Column, Database, Domain, EventTrigger, ForeignKey, Function, Index, KeyExpr, MaterializedView,
    Object, PUBLIC, Policy, QualifiedName, ReplicaIdentity, Role, Sequence, Table, Trigger,
    TriggerEnabled, View,
};
use crate::sql::{self, noun, relation_of};
use openmig_core::{Error, ObjectKind, ObjectRef, PlanMode};
use std::collections::{BTreeMap, BTreeSet};

fn r2(kind: ObjectKind, s: &str, n: &str) -> ObjectRef {
    ObjectRef::new(kind, [s, n])
}

fn r3(kind: ObjectKind, s: &str, t: &str, n: &str) -> ObjectRef {
    ObjectRef::new(kind, [s, t, n])
}

/// Whether `from` reaches `to` following foreign keys between the given tables.
fn fk_reaches(
    edges: &BTreeMap<QualifiedName, BTreeSet<QualifiedName>>,
    from: &QualifiedName,
    to: &QualifiedName,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from.clone()];
    while let Some(t) = stack.pop() {
        if &t == to {
            return true;
        }
        if !seen.insert(t.clone()) {
            continue;
        }
        if let Some(next) = edges.get(&t) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}

fn fk_graph(
    db: &Database,
    tables: &BTreeSet<QualifiedName>,
) -> BTreeMap<QualifiedName, BTreeSet<QualifiedName>> {
    let mut edges: BTreeMap<QualifiedName, BTreeSet<QualifiedName>> = BTreeMap::new();
    for t in tables {
        let Some(table) = db.table(t) else { continue };
        for fk in table.foreign_keys.values() {
            if &fk.ref_table != t && tables.contains(&fk.ref_table) {
                edges
                    .entry(t.clone())
                    .or_default()
                    .insert(fk.ref_table.clone());
            }
        }
    }
    edges
}

/// Foreign keys of new tables that can be written inside `CREATE TABLE`: self references,
/// references to tables that exist and stay, and references to other new tables outside a
/// foreign key cycle.
pub(super) fn inline_foreign_keys(
    des: &Database,
    new_tables: &BTreeSet<QualifiedName>,
    removed: &BTreeSet<ObjectRef>,
) -> BTreeSet<ObjectRef> {
    let edges = fk_graph(des, new_tables);
    let mut out = BTreeSet::new();
    for t in new_tables {
        let Some(table) = des.table(t) else { continue };
        for fk in table.foreign_keys.values() {
            let target = &fk.ref_table;
            let inline = if target == t {
                true
            } else if new_tables.contains(target) {
                !fk_reaches(&edges, target, t)
            } else {
                !removed.contains(&r2(ObjectKind::Table, &target.schema, &target.name))
            };
            if inline {
                out.insert(r3(ObjectKind::ForeignKey, &t.schema, &t.name, &fk.name));
            }
        }
    }
    out
}

/// Foreign keys between dropped tables that form a cycle; they are dropped before the tables.
pub(super) fn cyclic_foreign_keys(
    cur: &Database,
    dropped: &BTreeSet<QualifiedName>,
) -> BTreeSet<ObjectRef> {
    let edges = fk_graph(cur, dropped);
    let mut out = BTreeSet::new();
    for t in dropped {
        let Some(table) = cur.table(t) else { continue };
        for fk in table.foreign_keys.values() {
            if &fk.ref_table != t
                && dropped.contains(&fk.ref_table)
                && fk_reaches(&edges, &fk.ref_table, t)
            {
                out.insert(r3(ObjectKind::ForeignKey, &t.schema, &t.name, &fk.name));
            }
        }
    }
    out
}

impl Planner<'_> {
    fn des_deps(&self, r: &ObjectRef) -> Vec<ObjectRef> {
        self.des.dependencies_of(r).cloned().collect()
    }

    /// Requirements of a column: its type and what its default or expression uses.
    fn column_requires(&self, r: &ObjectRef, c: &Column) -> Vec<ObjectRef> {
        let mut out = type_refs(self.des, &c.data_type);
        out.extend(self.des_deps(r));
        out
    }

    fn column_hints(c: &Column) -> Vec<String> {
        c.default
            .iter()
            .cloned()
            .chain(c.generated.iter().map(|g| g.expr.clone()))
            .collect()
    }

    fn role_requires(&self, roles: &[String]) -> Vec<ObjectRef> {
        roles
            .iter()
            .filter(|r| r.as_str() != PUBLIC)
            .map(|r| ObjectRef::new(ObjectKind::Role, [r]))
            .collect()
    }

    /// Create `r` as the desired model defines it, with everything nested in it.
    pub(super) fn create(&mut self, r: &ObjectRef) -> Result<(), Error> {
        let des = self.des;
        let Some(object) = des.get(r) else {
            return Ok(());
        };
        let p = &r.path;
        match object {
            Object::Schema(s) => {
                let mut step = Step::new(r, Phase::Create, rank::SCHEMA, "");
                step.sql(sql::create_schema(&s.name), format!("Create {}", noun(r)));
                step.creates.push(r.clone());
                self.push(step);
                self.finish_create(r, s.comment.as_deref(), Some(&s.acl));
            }
            Object::Extension(e) => {
                let mut step = Step::new(r, Phase::Create, rank::EXTENSION, "");
                let version = (self.opts.mode == PlanMode::Scratch && !e.version.is_empty())
                    .then_some(e.version.as_str());
                step.sql(
                    sql::create_extension(&e.name, &e.schema, version),
                    format!("Create {}", noun(r)),
                );
                step.creates.push(r.clone());
                step.requires
                    .push(ObjectRef::new(ObjectKind::Schema, [&e.schema]));
                self.push(step);
                self.finish_create(r, e.comment.as_deref(), None);
            }
            Object::Role(role) => self.create_role(r, role),
            Object::EventTrigger(t) => self.create_event_trigger(r, t),
            Object::Table(t) => self.create_table(r, t),
            Object::Column(c) => self.create_column(r, c),
            Object::ViewColumn(_) | Object::CompositeField(_) | Object::Data(_) => {}
            Object::PrimaryKey(pk) => {
                let def = sql::primary_key_definition(pk);
                self.add_constraint(r, &def, &pk.columns, true, rank::CONSTRAINT);
                self.comment_if(r, pk.comment.as_deref());
            }
            Object::Unique(u) => {
                let def = sql::unique_definition(u);
                self.add_constraint(r, &def, &u.columns, true, rank::CONSTRAINT);
                self.comment_if(r, u.comment.as_deref());
            }
            Object::Check(c) => {
                let def = sql::check_definition(c);
                self.add_constraint(r, &def, &[], c.validated, rank::CONSTRAINT);
                if let Some(step) = self.steps.last_mut() {
                    step.hints.push(c.expr.clone());
                }
                self.comment_if(r, c.comment.as_deref());
            }
            Object::Exclusion(x) => {
                let def = sql::exclusion_definition(x);
                self.add_constraint(r, &def, &x.include, true, rank::CONSTRAINT);
                self.comment_if(r, x.comment.as_deref());
            }
            Object::ForeignKey(fk) => {
                self.add_foreign_key(r, fk);
                self.comment_if(r, fk.comment.as_deref());
            }
            Object::DomainCheck(c) => {
                let mut step = Step::new(r, Phase::Create, rank::CONSTRAINT, "");
                let action = format!(
                    "ADD {}{}",
                    sql::domain_check_definition(c),
                    if c.validated { "" } else { " NOT VALID" }
                );
                step.sql(
                    sql::alter_domain(&p[0], &p[1], &action),
                    format!("Add {}", noun(r)),
                );
                step.creates.push(r.clone());
                step.requires.push(r2(ObjectKind::Domain, &p[0], &p[1]));
                step.requires.extend(self.des_deps(r));
                step.hints.push(c.expr.clone());
                self.push(step);
                self.comment_if(r, c.comment.as_deref());
            }
            Object::Index(i) => self.create_index(r, i),
            Object::Policy(pol) => self.create_policy(r, pol),
            Object::Trigger(t) => self.create_trigger(r, t),
            Object::View(v) => self.create_view(r, v),
            Object::MaterializedView(m) => self.create_materialized_view(r, m),
            Object::Function(f) | Object::Procedure(f) => self.create_routine(r, f),
            Object::Sequence(s) => self.create_sequence(r, s),
            Object::Enum(e) => {
                let mut step = Step::new(r, Phase::Create, rank::ENUM, "");
                step.sql(
                    sql::create_enum(&p[0], &e.name, &e.values),
                    format!("Create {}", noun(r)),
                );
                step.creates.push(r.clone());
                step.requires
                    .push(ObjectRef::new(ObjectKind::Schema, [&p[0]]));
                self.push(step);
                self.finish_create(r, e.comment.as_deref(), Some(&e.acl));
            }
            Object::Composite(c) => {
                let mut step = Step::new(r, Phase::Create, rank::COMPOSITE, "");
                step.sql(
                    sql::create_composite(&p[0], c),
                    format!("Create {}", noun(r)),
                );
                step.creates.push(r.clone());
                step.creates.extend(nested(des, r));
                step.requires
                    .push(ObjectRef::new(ObjectKind::Schema, [&p[0]]));
                for f in &c.fields {
                    step.requires.extend(type_refs(des, &f.data_type));
                }
                self.push(step);
                self.finish_create(r, c.comment.as_deref(), Some(&c.acl));
            }
            Object::Domain(d) => self.create_domain(r, d),
        }
        Ok(())
    }

    /// Comment and privileges of a newly created object.
    fn finish_create(
        &mut self,
        r: &ObjectRef,
        comment: Option<&str>,
        acl: Option<&crate::model::Acl>,
    ) {
        self.comment_if(r, comment);
        if let Some(acl) = acl
            && let Some(delta) = acl_for_new(r.kind, acl, self.opts.scope, self.opts.major)
        {
            self.acl_step(r, &delta);
        }
    }

    pub(super) fn comment_if(&mut self, r: &ObjectRef, comment: Option<&str>) {
        if comment.is_some() {
            self.comment_step(r, comment);
        }
    }

    fn create_role(&mut self, r: &ObjectRef, role: &Role) {
        let mut step = Step::new(r, Phase::Create, rank::ROLE, "");
        step.sql(sql::create_role(role), format!("Create {}", noun(r)));
        step.creates.push(r.clone());
        self.push(step);
        for (group, m) in &role.member_of {
            self.membership_step(r, group, |major| {
                vec![sql::grant_membership(group, &role.name, m, major)]
            });
        }
        self.comment_if(r, role.comment.as_deref());
    }

    /// A step that changes `member`'s membership in `group`.
    pub(super) fn membership_step(
        &mut self,
        member: &ObjectRef,
        group: &str,
        statements: impl FnOnce(u32) -> Vec<String>,
    ) {
        let mut step = Step::new(
            member,
            Phase::Create,
            rank::MEMBERSHIP,
            &format!(" in {group}"),
        );
        for s in statements(self.opts.major) {
            step.sql(
                s,
                format!(
                    "Set membership of {} in role {}",
                    noun(member),
                    sql::ident(group)
                ),
            );
        }
        step.requires.push(member.clone());
        step.requires
            .push(ObjectRef::new(ObjectKind::Role, [group]));
        self.push(step);
    }

    fn create_event_trigger(&mut self, r: &ObjectRef, t: &EventTrigger) {
        let mut step = Step::new(r, Phase::Create, rank::EVENT_TRIGGER, "");
        step.sql(sql::create_event_trigger(t), format!("Create {}", noun(r)));
        if t.enabled != TriggerEnabled::Origin {
            step.sql(
                sql::event_trigger_enabled(&t.name, t.enabled),
                format!("Set firing of {}", noun(r)),
            );
        }
        step.creates.push(r.clone());
        step.requires.extend(functions_named(self.des, &t.function));
        step.requires.extend(self.des_deps(r));
        self.push(step);
        self.comment_if(r, t.comment.as_deref());
    }

    fn create_table(&mut self, r: &ObjectRef, t: &Table) {
        let des = self.des;
        let name = relation_of(r);
        let (s, tn) = (&name.schema, &name.name);
        let inherited = if t.partition_of.is_some() {
            BTreeSet::new()
        } else {
            inherited_columns(des, t)
        };
        let columns: Vec<&Column> = t
            .columns
            .iter()
            .filter(|c| !inherited.contains(&c.name))
            .collect();
        let inline: Vec<&ForeignKey> = t
            .foreign_keys
            .values()
            .filter(|fk| {
                self.inline_fks
                    .contains(&r3(ObjectKind::ForeignKey, s, tn, &fk.name))
            })
            .collect();
        let mut step = Step::new(r, Phase::Create, rank::TABLE, "");
        step.sql(
            sql::create_table(&name, t, &columns, &inline),
            format!("Create {}", noun(r)),
        );
        for c in &columns {
            if let Some(storage) = c.storage {
                step.sql(
                    sql::alter_column(&name, &c.name, &format!("SET STORAGE {}", storage.as_sql())),
                    format!(
                        "Set storage of column {} of {}",
                        sql::ident(&c.name),
                        noun(r)
                    ),
                );
            }
        }
        step.creates.push(r.clone());
        step.creates.extend(
            t.columns
                .iter()
                .map(|c| r3(ObjectKind::Column, s, tn, &c.name)),
        );
        if let Some(pk) = &t.primary_key {
            step.creates
                .push(r3(ObjectKind::PrimaryKey, s, tn, &pk.name));
        }
        let inline_uniques = sql::inline_uniques(t);
        step.creates.extend(
            inline_uniques
                .iter()
                .map(|u| r3(ObjectKind::Unique, s, tn, &u.name)),
        );
        step.creates.extend(
            t.checks
                .values()
                .filter(|c| c.validated)
                .map(|c| r3(ObjectKind::Check, s, tn, &c.name)),
        );
        step.creates.extend(
            t.exclusions
                .keys()
                .map(|n| r3(ObjectKind::Exclusion, s, tn, n)),
        );
        step.requires.push(ObjectRef::new(ObjectKind::Schema, [s]));
        step.requires.extend(self.des_deps(r));
        for c in &t.columns {
            let cr = r3(ObjectKind::Column, s, tn, &c.name);
            step.requires.extend(self.column_requires(&cr, c));
            step.hints.extend(Self::column_hints(c));
        }
        for c in t.checks.values().filter(|c| c.validated) {
            step.requires
                .extend(self.des_deps(&r3(ObjectKind::Check, s, tn, &c.name)));
            step.hints.push(c.expr.clone());
        }
        for x in t.exclusions.values() {
            step.requires
                .extend(self.des_deps(&r3(ObjectKind::Exclusion, s, tn, &x.name)));
        }
        if let Some(p) = &t.partition_of {
            step.requires
                .push(r2(ObjectKind::Table, &p.parent.schema, &p.parent.name));
        }
        for p in &t.inherits {
            step.requires
                .push(r2(ObjectKind::Table, &p.schema, &p.name));
        }
        for fk in &inline {
            let fr = r3(ObjectKind::ForeignKey, s, tn, &fk.name);
            step.creates.push(fr.clone());
            if fk.ref_table != name {
                step.requires.extend(self.foreign_key_requires(&fr, fk));
            }
        }
        self.push(step);

        for u in t.uniques.values() {
            if !inline_uniques.iter().any(|i| i.name == u.name) {
                let ur = r3(ObjectKind::Unique, s, tn, &u.name);
                self.add_constraint(
                    &ur,
                    &sql::unique_definition(u),
                    &u.columns,
                    true,
                    rank::CONSTRAINT,
                );
            }
        }
        for c in t.checks.values().filter(|c| !c.validated) {
            let cr = r3(ObjectKind::Check, s, tn, &c.name);
            self.add_constraint(&cr, &sql::check_definition(c), &[], false, rank::CONSTRAINT);
        }
        for fk in t.foreign_keys.values() {
            let fr = r3(ObjectKind::ForeignKey, s, tn, &fk.name);
            if !self.inline_fks.contains(&fr) {
                self.add_foreign_key(&fr, fk);
            }
        }
        for i in t.standalone_indexes() {
            self.create_index(&r3(ObjectKind::Index, s, tn, &i.name), i);
        }
        for p in t.policies.values() {
            self.create_policy(&r3(ObjectKind::Policy, s, tn, &p.name), p);
        }
        for g in t.triggers.values() {
            self.create_trigger(&r3(ObjectKind::Trigger, s, tn, &g.name), g);
        }
        if t.rls_enabled || t.rls_forced {
            let mut step = Step::new(r, Phase::Create, rank::RLS, " rls");
            if t.rls_enabled {
                step.sql(
                    sql::alter_table(&name, "ENABLE ROW LEVEL SECURITY"),
                    format!("Enable row level security on {}", noun(r)),
                );
            }
            if t.rls_forced {
                step.sql(
                    sql::alter_table(&name, "FORCE ROW LEVEL SECURITY"),
                    format!("Force row level security on {}", noun(r)),
                );
            }
            step.requires.push(r.clone());
            self.push(step);
        }
        if t.replica_identity != ReplicaIdentity::Default {
            self.replica_identity_step(r, &t.replica_identity);
        }
        self.comment_if(r, t.comment.as_deref());
        for c in &t.columns {
            let cr = r3(ObjectKind::Column, s, tn, &c.name);
            self.comment_if(&cr, c.comment.as_deref());
            if let Some(delta) =
                acl_for_new(ObjectKind::Column, &c.acl, self.opts.scope, self.opts.major)
            {
                self.acl_step(&cr, &delta);
            }
        }
        if let Some(pk) = &t.primary_key {
            self.comment_if(
                &r3(ObjectKind::PrimaryKey, s, tn, &pk.name),
                pk.comment.as_deref(),
            );
        }
        for u in t.uniques.values() {
            self.comment_if(
                &r3(ObjectKind::Unique, s, tn, &u.name),
                u.comment.as_deref(),
            );
        }
        for c in t.checks.values() {
            self.comment_if(&r3(ObjectKind::Check, s, tn, &c.name), c.comment.as_deref());
        }
        for x in t.exclusions.values() {
            self.comment_if(
                &r3(ObjectKind::Exclusion, s, tn, &x.name),
                x.comment.as_deref(),
            );
        }
        for fk in t.foreign_keys.values() {
            self.comment_if(
                &r3(ObjectKind::ForeignKey, s, tn, &fk.name),
                fk.comment.as_deref(),
            );
        }
        if let Some(delta) =
            acl_for_new(ObjectKind::Table, &t.acl, self.opts.scope, self.opts.major)
        {
            self.acl_step(r, &delta);
        }
    }

    pub(super) fn replica_identity_step(&mut self, r: &ObjectRef, ri: &ReplicaIdentity) {
        let name = relation_of(r);
        let mut step = Step::new(
            r,
            Phase::Create,
            rank::REPLICA_IDENTITY,
            " replica identity",
        );
        step.sql(
            sql::alter_table(&name, &sql::replica_identity(ri)),
            format!("Set replica identity of {}", noun(r)),
        );
        step.requires.push(r.clone());
        if let ReplicaIdentity::Index(i) = ri {
            for kind in [
                ObjectKind::Index,
                ObjectKind::PrimaryKey,
                ObjectKind::Unique,
            ] {
                step.requires.push(r3(kind, &name.schema, &name.name, i));
            }
        }
        self.push(step);
    }

    fn create_column(&mut self, r: &ObjectRef, c: &Column) {
        let table = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::COLUMN, "");
        step.sql(
            sql::add_column(&table, c),
            format!(
                "Add column {} to table {}",
                sql::ident(&c.name),
                table.to_sql()
            ),
        );
        if let Some(storage) = c.storage {
            step.sql(
                sql::alter_column(
                    &table,
                    &c.name,
                    &format!("SET STORAGE {}", storage.as_sql()),
                ),
                format!("Set storage of {}", noun(r)),
            );
        }
        step.creates.push(r.clone());
        step.requires
            .push(r2(ObjectKind::Table, &table.schema, &table.name));
        step.requires.extend(self.column_requires(r, c));
        step.hints.extend(Self::column_hints(c));
        self.push(step);
        self.comment_if(r, c.comment.as_deref());
        if let Some(delta) =
            acl_for_new(ObjectKind::Column, &c.acl, self.opts.scope, self.opts.major)
        {
            self.acl_step(r, &delta);
        }
    }

    fn add_constraint(
        &mut self,
        r: &ObjectRef,
        definition: &str,
        columns: &[String],
        valid: bool,
        rank: u16,
    ) {
        let table = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank, "");
        step.sql(
            sql::add_constraint(&table, definition, !valid),
            format!("Add {}", noun(r)),
        );
        step.creates.push(r.clone());
        step.requires
            .push(r2(ObjectKind::Table, &table.schema, &table.name));
        step.requires.extend(
            columns
                .iter()
                .map(|c| r3(ObjectKind::Column, &table.schema, &table.name, c)),
        );
        step.requires.extend(self.des_deps(r));
        self.push(step);
    }

    fn foreign_key_requires(&self, r: &ObjectRef, fk: &ForeignKey) -> Vec<ObjectRef> {
        let t = &fk.ref_table;
        let mut out = vec![r2(ObjectKind::Table, &t.schema, &t.name)];
        out.extend(
            fk.ref_columns
                .iter()
                .map(|c| r3(ObjectKind::Column, &t.schema, &t.name, c)),
        );
        out.extend(referenced_key(self.des, fk));
        out.extend(self.des_deps(r));
        out
    }

    fn add_foreign_key(&mut self, r: &ObjectRef, fk: &ForeignKey) {
        self.add_constraint(
            r,
            &sql::foreign_key_definition(fk),
            &fk.columns,
            fk.validated,
            rank::FOREIGN_KEY,
        );
        let requires = self.foreign_key_requires(r, fk);
        if let Some(step) = self.steps.last_mut() {
            step.requires.extend(requires);
        }
    }

    /// Whether index changes on the relation of `r` use `CONCURRENTLY`: the setting is on, the
    /// relation exists before and after the plan, is not partitioned, and this is not a scratch
    /// build.
    pub(super) fn concurrently(&self, r: &ObjectRef) -> bool {
        if !self.opts.settings.concurrent_indexes || self.scratch() {
            return false;
        }
        let rel = relation_of(r);
        let table = r2(ObjectKind::Table, &rel.schema, &rel.name);
        let matview = r2(ObjectKind::MaterializedView, &rel.schema, &rel.name);
        if self.new_tables.contains(&rel)
            || self.removed.contains(&table)
            || self.removed.contains(&matview)
        {
            return false;
        }
        match (self.cur.get(&table), self.cur.get(&matview)) {
            (Some(Object::Table(t)), _) => t.partition_by.is_none(),
            (_, Some(Object::MaterializedView(_))) => self.des.get(&matview).is_some(),
            _ => false,
        }
    }

    pub(super) fn create_index(&mut self, r: &ObjectRef, i: &Index) {
        let rel = relation_of(r);
        let concurrently = self.concurrently(r);
        let rank = if self.des.table(&rel).is_some() {
            rank::INDEX
        } else {
            rank::MATERIALIZED_VIEW + 1
        };
        let mut step = Step::new(r, Phase::Create, rank, "");
        let comment = format!("Create index {} on {}", sql::ident(&i.name), rel.to_sql());
        if concurrently && self.opts.mode == PlanMode::Generate {
            step.sql(
                sql::drop_index(&rel.schema, &i.name, true, true),
                comment.clone(),
            )
            .tx = openmig_core::TxRequirement::Forbidden;
        }
        let st = step.sql(sql::create_index(&rel, i, concurrently), comment);
        if concurrently {
            st.tx = openmig_core::TxRequirement::Forbidden;
        }
        step.creates.push(r.clone());
        for kind in [ObjectKind::Table, ObjectKind::MaterializedView] {
            step.requires.push(r2(kind, &rel.schema, &rel.name));
        }
        for k in &i.keys {
            match &k.expr {
                KeyExpr::Column(c) => {
                    step.requires
                        .push(r3(ObjectKind::Column, &rel.schema, &rel.name, c))
                }
                KeyExpr::Expression(e) => step.hints.push(e.clone()),
            }
        }
        step.requires.extend(
            i.include
                .iter()
                .map(|c| r3(ObjectKind::Column, &rel.schema, &rel.name, c)),
        );
        step.hints.extend(i.predicate.iter().cloned());
        step.requires.extend(self.des_deps(r));
        self.push(step);
        self.comment_if(r, i.comment.as_deref());
    }

    fn create_policy(&mut self, r: &ObjectRef, p: &Policy) {
        let table = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::POLICY, "");
        step.sql(sql::create_policy(&table, p), format!("Create {}", noun(r)));
        step.creates.push(r.clone());
        step.requires
            .push(r2(ObjectKind::Table, &table.schema, &table.name));
        step.requires.extend(self.role_requires(&p.roles));
        step.requires.extend(self.des_deps(r));
        step.hints.extend(p.using.iter().cloned());
        step.hints.extend(p.check.iter().cloned());
        self.push(step);
        self.comment_if(r, p.comment.as_deref());
    }

    fn create_trigger(&mut self, r: &ObjectRef, t: &Trigger) {
        let rel = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::TRIGGER, "");
        step.sql(
            sql::create_trigger(&rel, t, false),
            format!("Create {}", noun(r)),
        );
        if t.enabled != TriggerEnabled::Origin {
            step.sql(
                sql::trigger_enabled(&rel, &t.name, t.enabled),
                format!("Set firing of {}", noun(r)),
            );
        }
        step.creates.push(r.clone());
        for kind in [ObjectKind::Table, ObjectKind::View] {
            step.requires.push(r2(kind, &rel.schema, &rel.name));
        }
        step.requires.extend(
            t.update_of
                .iter()
                .map(|c| r3(ObjectKind::Column, &rel.schema, &rel.name, c)),
        );
        step.requires.extend(functions_named(self.des, &t.function));
        if let Some(c) = &t.constraint
            && let Some(from) = &c.from
        {
            step.requires
                .push(r2(ObjectKind::Table, &from.schema, &from.name));
        }
        step.requires.extend(self.des_deps(r));
        step.hints.extend(t.when.iter().cloned());
        self.push(step);
        self.comment_if(r, t.comment.as_deref());
    }

    fn view_columns_finish(&mut self, r: &ObjectRef, columns: &[crate::model::ViewColumn]) {
        for c in columns {
            let cr = r3(ObjectKind::Column, &r.path[0], &r.path[1], &c.name);
            self.comment_if(&cr, c.comment.as_deref());
            if let Some(delta) =
                acl_for_new(ObjectKind::Column, &c.acl, self.opts.scope, self.opts.major)
            {
                self.acl_step(&cr, &delta);
            }
        }
    }

    fn create_view(&mut self, r: &ObjectRef, v: &View) {
        let name = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::VIEW, "");
        step.sql(
            sql::create_view(&name, v, false),
            format!("Create {}", noun(r)),
        );
        step.creates.push(r.clone());
        step.creates.extend(
            v.columns
                .iter()
                .map(|c| r3(ObjectKind::Column, &name.schema, &name.name, &c.name)),
        );
        step.requires
            .push(ObjectRef::new(ObjectKind::Schema, [&name.schema]));
        step.requires.extend(self.des_deps(r));
        step.hints.push(v.query.clone());
        self.push(step);
        for g in v.triggers.values() {
            self.create_trigger(
                &r3(ObjectKind::Trigger, &name.schema, &name.name, &g.name),
                g,
            );
        }
        self.finish_create(r, v.comment.as_deref(), Some(&v.acl));
        self.view_columns_finish(r, &v.columns);
    }

    fn create_materialized_view(&mut self, r: &ObjectRef, m: &MaterializedView) {
        let name = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::MATERIALIZED_VIEW, "");
        step.sql(
            sql::create_materialized_view(&name, m),
            format!("Create {}", noun(r)),
        );
        step.creates.push(r.clone());
        step.creates.extend(
            m.columns
                .iter()
                .map(|c| r3(ObjectKind::Column, &name.schema, &name.name, &c.name)),
        );
        step.requires
            .push(ObjectRef::new(ObjectKind::Schema, [&name.schema]));
        step.requires.extend(self.des_deps(r));
        step.hints.push(m.query.clone());
        self.push(step);
        for i in m.indexes.values() {
            self.create_index(&r3(ObjectKind::Index, &name.schema, &name.name, &i.name), i);
        }
        self.finish_create(r, m.comment.as_deref(), Some(&m.acl));
        self.view_columns_finish(r, &m.columns);
    }

    /// What a function needs besides its schema: argument and result types and the objects a
    /// SQL-standard body uses.
    pub(super) fn routine_requires(&self, r: &ObjectRef, f: &Function) -> Vec<ObjectRef> {
        let mut out = vec![ObjectRef::new(ObjectKind::Schema, [&r.path[0]])];
        for a in &f.args {
            out.extend(type_refs(self.des, &a.data_type));
        }
        match &f.returns {
            Some(crate::model::Returns::Type { data_type, .. }) => {
                out.extend(type_refs(self.des, data_type));
            }
            Some(crate::model::Returns::Table(cols)) => {
                for c in cols {
                    out.extend(type_refs(self.des, &c.data_type));
                }
            }
            None => {}
        }
        out.extend(self.des_deps(r));
        out
    }

    pub(super) fn routine_hints(f: &Function) -> Vec<String> {
        let mut out: Vec<String> = f.args.iter().filter_map(|a| a.default.clone()).collect();
        out.push(f.body.text().to_owned());
        out
    }

    fn create_routine(&mut self, r: &ObjectRef, f: &Function) {
        let mut step = Step::new(r, Phase::Create, rank::FUNCTION, "");
        step.sql(
            sql::create_routine(&r.path[0], f, false),
            format!("Create {}", noun(r)),
        );
        step.creates.push(r.clone());
        step.requires.extend(self.routine_requires(r, f));
        step.hints.extend(Self::routine_hints(f));
        self.push(step);
        self.finish_create(r, f.comment.as_deref(), Some(&f.acl));
    }

    fn create_sequence(&mut self, r: &ObjectRef, s: &Sequence) {
        let schema = &r.path[0];
        let mut step = Step::new(r, Phase::Create, rank::SEQUENCE, "");
        step.sql(
            sql::create_sequence(schema, s),
            format!("Create {}", noun(r)),
        );
        step.creates.push(r.clone());
        step.requires
            .push(ObjectRef::new(ObjectKind::Schema, [schema]));
        self.push(step);
        if s.owned_by.is_some() {
            self.owned_by_step(r, s);
        }
        self.finish_create(r, s.comment.as_deref(), Some(&s.acl));
    }

    pub(super) fn owned_by_step(&mut self, r: &ObjectRef, s: &Sequence) {
        let mut step = Step::new(r, Phase::Create, rank::OWNED_BY, " owned by");
        step.sql(
            sql::sequence_owned_by(&r.path[0], &s.name, s.owned_by.as_ref()),
            format!("Set owner column of {}", noun(r)),
        );
        step.requires.push(r.clone());
        if let Some(o) = &s.owned_by {
            step.requires
                .push(r3(ObjectKind::Column, &o.schema, &o.table, &o.column));
        }
        step.releases.push(r.clone());
        self.push(step);
    }

    fn create_domain(&mut self, r: &ObjectRef, d: &Domain) {
        let schema = &r.path[0];
        let mut step = Step::new(r, Phase::Create, rank::DOMAIN, "");
        step.sql(sql::create_domain(schema, d), format!("Create {}", noun(r)));
        step.creates.push(r.clone());
        step.creates.extend(
            d.checks
                .values()
                .filter(|c| c.validated)
                .map(|c| r3(ObjectKind::Check, schema, &d.name, &c.name)),
        );
        step.requires
            .push(ObjectRef::new(ObjectKind::Schema, [schema]));
        step.requires.extend(type_refs(self.des, &d.data_type));
        step.requires.extend(self.des_deps(r));
        step.hints.extend(d.default.iter().cloned());
        for c in d.checks.values() {
            step.requires
                .extend(self.des_deps(&r3(ObjectKind::Check, schema, &d.name, &c.name)));
            step.hints.push(c.expr.clone());
        }
        self.push(step);
        for c in d.checks.values() {
            let cr = r3(ObjectKind::Check, schema, &d.name, &c.name);
            if !c.validated {
                let mut step = Step::new(&cr, Phase::Create, rank::CONSTRAINT, "");
                step.sql(
                    sql::alter_domain(
                        schema,
                        &d.name,
                        &format!("ADD {} NOT VALID", sql::domain_check_definition(c)),
                    ),
                    format!("Add {}", noun(&cr)),
                );
                step.creates.push(cr.clone());
                step.requires.push(r.clone());
                self.push(step);
            }
            self.comment_if(&cr, c.comment.as_deref());
        }
        self.finish_create(r, d.comment.as_deref(), Some(&d.acl));
    }

    /// Drop the current definition of `r`.
    pub(super) fn drop(&mut self, r: &ObjectRef) -> Result<(), Error> {
        let cur = self.cur;
        let Some(object) = cur.get(r) else {
            return Ok(());
        };
        let p = &r.path;
        let mut step = Step::new(r, Phase::Drop, drop_rank(r.kind), " drop");
        step.removes.push(r.clone());
        step.removes.extend(
            nested(cur, r)
                .into_iter()
                .filter(|n| !self.split_fks.contains(n)),
        );
        step.releases = step.removes.clone();
        let comment = format!("Drop {}", noun(r));
        let statement = match object {
            Object::Schema(s) => Some(sql::drop_schema(&s.name)),
            Object::Extension(e) => Some(sql::drop_extension(&e.name)),
            Object::Role(role) => Some(sql::drop_role(&role.name)),
            Object::EventTrigger(t) => Some(sql::drop_event_trigger(&t.name)),
            Object::Table(t) => {
                for fk in t.foreign_keys.keys() {
                    let fr = r3(ObjectKind::ForeignKey, &p[0], &p[1], fk);
                    if self.split_fks.contains(&fr) {
                        self.drop_constraint_step(&fr);
                    }
                }
                Some(sql::drop_table(&relation_of(r)))
            }
            Object::Column(c) => Some(sql::drop_column(&relation_of(r), &c.name)),
            Object::ViewColumn(_) | Object::CompositeField(_) | Object::Data(_) => None,
            Object::PrimaryKey(_)
            | Object::Unique(_)
            | Object::Check(_)
            | Object::Exclusion(_)
            | Object::ForeignKey(_) => Some(sql::drop_constraint(&relation_of(r), r.name())),
            Object::DomainCheck(c) => Some(sql::alter_domain(
                &p[0],
                &p[1],
                &format!("DROP CONSTRAINT {}", sql::ident(&c.name)),
            )),
            Object::Index(i) => {
                let concurrently = self.concurrently(r);
                let st = step.sql(
                    sql::drop_index(&p[0], &i.name, concurrently, false),
                    comment.clone(),
                );
                if concurrently {
                    st.tx = openmig_core::TxRequirement::Forbidden;
                }
                None
            }
            Object::Policy(pol) => Some(sql::drop_policy(&relation_of(r), &pol.name)),
            Object::Trigger(t) => Some(sql::drop_trigger(&relation_of(r), &t.name)),
            Object::View(_) => Some(sql::drop_view(&relation_of(r))),
            Object::MaterializedView(_) => Some(sql::drop_materialized_view(&relation_of(r))),
            Object::Function(f) | Object::Procedure(f) => Some(sql::drop_routine(&p[0], f)),
            Object::Sequence(s) => {
                // `DROP TABLE` and `DROP COLUMN` drop the sequences their columns own.
                let implicit = s.owned_by.as_ref().is_some_and(|o| {
                    self.removed
                        .contains(&r3(ObjectKind::Column, &o.schema, &o.table, &o.column))
                });
                (!implicit).then(|| sql::drop_sequence(&p[0], &s.name))
            }
            Object::Enum(e) => Some(sql::drop_type(&p[0], &e.name)),
            Object::Composite(c) => Some(sql::drop_type(&p[0], &c.name)),
            Object::Domain(d) => Some(sql::drop_domain(&p[0], &d.name)),
        };
        if let Some(statement) = statement {
            let st = step.sql(statement, comment);
            st.destructive = drop_destroys(r);
        }
        self.push(step);
        Ok(())
    }

    fn drop_constraint_step(&mut self, r: &ObjectRef) {
        let mut step = Step::new(r, Phase::Drop, drop_rank(r.kind), " drop");
        step.sql(
            sql::drop_constraint(&relation_of(r), r.name()),
            format!("Drop {}", noun(r)),
        );
        step.removes.push(r.clone());
        step.releases.push(r.clone());
        self.push(step);
    }

    /// Rename (and move) a table or rename a column. The current model is already re-keyed.
    pub(super) fn rename(&mut self, from: &ObjectRef, to: &ObjectRef) {
        let mut step = Step::new(to, Phase::Create, rank::RENAME, " rename");
        step.kind = StepKind::Rename;
        match (from.path.as_slice(), to.path.as_slice()) {
            ([fs, fname], [ts, tname]) => {
                let mut name = QualifiedName::new(fs, fname);
                if fs != ts {
                    step.sql(
                        sql::set_table_schema(&name, ts),
                        format!("Move {} to schema {}", noun(from), sql::ident(ts)),
                    );
                    step.requires.push(ObjectRef::new(ObjectKind::Schema, [ts]));
                    name = QualifiedName::new(ts, fname);
                }
                if fname != tname {
                    step.sql(
                        sql::rename_table(&name, tname),
                        format!("Rename table {} to {}", name.to_sql(), sql::ident(tname)),
                    );
                }
                step.creates.extend(nested(self.cur, to));
            }
            ([_, _, fc], [ts, tt, tc]) => {
                let table = QualifiedName::new(ts, tt);
                step.sql(
                    sql::rename_column(&table, fc, tc),
                    format!(
                        "Rename column {} of table {} to {}",
                        sql::ident(fc),
                        table.to_sql(),
                        sql::ident(tc)
                    ),
                );
                step.requires.push(r2(ObjectKind::Table, ts, tt));
            }
            _ => return,
        }
        step.creates.push(to.clone());
        self.push(step);
    }
}
