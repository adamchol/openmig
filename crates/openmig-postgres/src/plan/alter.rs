//! Steps for in-place changes ([`AlterOp`]).

use super::Planner;
use super::refs::{functions_named, type_refs};
use super::step::{Phase, Step, StepKind, rank};
use crate::diff::AlterOp;
use crate::model::{
    Column, Composite, Domain, Function, Identity, MaterializedView, Membership, Object, Policy,
    Role, Sequence, Storage, Table, Trigger, TriggerEnabled, View,
};
use crate::sql::{self, noun, relation_of};
use openmig_core::{Error, ObjectKind, ObjectRef};

fn r2(kind: ObjectKind, s: &str, n: &str) -> ObjectRef {
    ObjectRef::new(kind, [s, n])
}

/// The comment of any model object.
pub(super) fn object_comment(o: Object<'_>) -> Option<&str> {
    match o {
        Object::Schema(x) => x.comment.as_deref(),
        Object::Extension(x) => x.comment.as_deref(),
        Object::Role(x) => x.comment.as_deref(),
        Object::EventTrigger(x) => x.comment.as_deref(),
        Object::Table(x) => x.comment.as_deref(),
        Object::Column(x) => x.comment.as_deref(),
        Object::ViewColumn(x) => x.comment.as_deref(),
        Object::CompositeField(_) | Object::Data(_) => None,
        Object::PrimaryKey(x) => x.comment.as_deref(),
        Object::ForeignKey(x) => x.comment.as_deref(),
        Object::Unique(x) => x.comment.as_deref(),
        Object::Check(x) => x.comment.as_deref(),
        Object::DomainCheck(x) => x.comment.as_deref(),
        Object::Exclusion(x) => x.comment.as_deref(),
        Object::Index(x) => x.comment.as_deref(),
        Object::Policy(x) => x.comment.as_deref(),
        Object::Trigger(x) => x.comment.as_deref(),
        Object::View(x) => x.comment.as_deref(),
        Object::MaterializedView(x) => x.comment.as_deref(),
        Object::Function(x) | Object::Procedure(x) => x.comment.as_deref(),
        Object::Enum(x) => x.comment.as_deref(),
        Object::Composite(x) => x.comment.as_deref(),
        Object::Domain(x) => x.comment.as_deref(),
        Object::Sequence(x) => x.comment.as_deref(),
    }
}

/// `SET STORAGE` value that restores a type's default storage. PostgreSQL 16 accepts `DEFAULT`;
/// older servers need the type's own default, guessed from its name.
fn default_storage(data_type: &str, major: u32) -> &'static str {
    if major >= 16 {
        return "DEFAULT";
    }
    let base = data_type.split('(').next().unwrap_or(data_type).trim();
    if data_type.ends_with(']') {
        return Storage::Extended.as_sql();
    }
    match base {
        "numeric" => Storage::Main.as_sql(),
        "text" | "bytea" | "jsonb" | "json" | "xml" | "character varying" | "character"
        | "tsvector" | "tsquery" | "public.citext" => Storage::Extended.as_sql(),
        _ => Storage::Plain.as_sql(),
    }
}

impl Planner<'_> {
    pub(super) fn alter(&mut self, r: &ObjectRef, ops: &[AlterOp]) -> Result<(), Error> {
        let (Some(cur), Some(des)) = (self.cur.get(r), self.des.get(r)) else {
            return Ok(());
        };
        for op in ops {
            match op {
                AlterOp::Comment => self.comment_step(r, object_comment(des)),
                AlterOp::Acl(delta) => self.acl_step(r, delta),
                _ => {}
            }
        }
        let has = |op: &AlterOp| ops.contains(op);
        match (cur, des) {
            (Object::Role(c), Object::Role(d)) => self.alter_role(r, c, d, ops),
            (Object::Extension(_), Object::Extension(d)) if has(&AlterOp::ExtensionSchema) => {
                let mut step = Step::new(r, Phase::Create, rank::EXTENSION, " schema");
                step.sql(
                    sql::alter_extension_schema(&d.name, &d.schema),
                    format!("Move {} to schema {}", noun(r), sql::ident(&d.schema)),
                );
                step.requires
                    .push(ObjectRef::new(ObjectKind::Schema, [&d.schema]));
                step.creates.push(r.clone());
                self.push(step);
            }
            (Object::EventTrigger(_), Object::EventTrigger(d)) if has(&AlterOp::Enabled) => {
                let mut step = Step::new(r, Phase::Create, rank::EVENT_TRIGGER, " enabled");
                step.sql(
                    sql::event_trigger_enabled(&d.name, d.enabled),
                    format!("Set firing of {}", noun(r)),
                );
                self.push(step);
            }
            (Object::Table(c), Object::Table(d)) => self.alter_table(r, c, d, ops),
            (Object::Column(c), Object::Column(d)) => self.alter_column(r, c, d, ops),
            (
                Object::ForeignKey(_)
                | Object::Check(_)
                | Object::PrimaryKey(_)
                | Object::Unique(_),
                _,
            ) if has(&AlterOp::Validate) => {
                let mut step = Step::new(r, Phase::Create, rank::CONSTRAINT, " validate");
                step.sql(
                    sql::validate_constraint(&relation_of(r), r.name()),
                    format!("Validate {}", noun(r)),
                );
                self.push(step);
            }
            (Object::DomainCheck(_), _) if has(&AlterOp::Validate) => {
                let mut step = Step::new(r, Phase::Create, rank::CONSTRAINT, " validate");
                step.sql(
                    sql::alter_domain(
                        &r.path[0],
                        &r.path[1],
                        &format!("VALIDATE CONSTRAINT {}", sql::ident(r.name())),
                    ),
                    format!("Validate {}", noun(r)),
                );
                self.push(step);
            }
            (Object::Policy(c), Object::Policy(d)) if has(&AlterOp::PolicyDefinition) => {
                self.alter_policy(r, c, d);
            }
            (Object::Trigger(_), Object::Trigger(d)) => self.alter_trigger(r, d, ops),
            (Object::View(c), Object::View(d)) => self.alter_view(r, c, d, ops),
            (Object::MaterializedView(c), Object::MaterializedView(d)) => {
                self.alter_materialized_view(r, c, d, ops);
            }
            (Object::Function(c), Object::Function(d))
            | (Object::Procedure(c), Object::Procedure(d)) => self.alter_routine(r, c, d, ops),
            (Object::Sequence(c), Object::Sequence(d)) => self.alter_sequence(r, c, d, ops),
            (Object::Enum(_), Object::Enum(e)) => {
                let mut step = Step::new(r, Phase::Create, rank::ENUM, " values");
                step.kind = StepKind::EnumValues;
                for op in ops {
                    if let AlterOp::AddValue { value, position } = op {
                        step.sql(
                            sql::add_enum_value(&r.path[0], &e.name, value, position),
                            format!("Add value {} to {}", sql::literal(value), noun(r)),
                        );
                    }
                }
                if !step.statements.is_empty() {
                    step.creates.push(r.clone());
                    self.push(step);
                }
            }
            (Object::Composite(c), Object::Composite(d)) => self.alter_composite(r, c, d, ops),
            (Object::Domain(c), Object::Domain(d)) => self.alter_domain(r, c, d, ops),
            _ => {}
        }
        Ok(())
    }

    fn alter_role(&mut self, r: &ObjectRef, cur: &Role, des: &Role, ops: &[AlterOp]) {
        if ops.contains(&AlterOp::RoleAttributes)
            && let Some(statement) = sql::alter_role(cur, des)
        {
            let mut step = Step::new(r, Phase::Create, rank::ROLE, " attributes");
            step.sql(statement, format!("Alter {}", noun(r)));
            self.push(step);
        }
        for op in ops {
            match op {
                AlterOp::GrantMembership(group) => {
                    let Some(m) = des.member_of.get(group).copied() else {
                        continue;
                    };
                    let existing = cur.member_of.get(group).copied();
                    let member = des.name.clone();
                    self.membership_step(r, group, |major| {
                        membership_statements(group, &member, existing, m, major)
                    });
                }
                AlterOp::RevokeMembership(group) => {
                    let mut step =
                        Step::new(r, Phase::Create, rank::MEMBERSHIP, &format!(" in {group}"));
                    step.sql(
                        sql::revoke_membership(group, &des.name, None),
                        format!(
                            "Revoke membership of {} in role {}",
                            noun(r),
                            sql::ident(group)
                        ),
                    );
                    self.push(step);
                }
                _ => {}
            }
        }
    }

    fn alter_table(&mut self, r: &ObjectRef, cur: &Table, des: &Table, ops: &[AlterOp]) {
        let name = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::TABLE_ALTER, " alter");
        step.requires.push(r.clone());
        for op in ops {
            match op {
                AlterOp::Partition => {
                    if let Some(p) = &cur.partition_of {
                        step.sql(
                            sql::alter_table(
                                &p.parent,
                                &format!("DETACH PARTITION {}", name.to_sql()),
                            ),
                            format!("Detach {} from {}", noun(r), p.parent.to_sql()),
                        );
                    }
                    if let Some(p) = &des.partition_of {
                        step.sql(
                            sql::alter_table(
                                &p.parent,
                                &format!("ATTACH PARTITION {} {}", name.to_sql(), p.bound),
                            ),
                            format!("Attach {} to {}", noun(r), p.parent.to_sql()),
                        );
                        step.requires
                            .push(r2(ObjectKind::Table, &p.parent.schema, &p.parent.name));
                    }
                }
                AlterOp::Inherit(p) => {
                    step.sql(
                        sql::alter_table(&name, &format!("INHERIT {}", p.to_sql())),
                        format!("Make {} inherit from {}", noun(r), p.to_sql()),
                    );
                    step.requires
                        .push(r2(ObjectKind::Table, &p.schema, &p.name));
                }
                AlterOp::NoInherit(p) => {
                    step.sql(
                        sql::alter_table(&name, &format!("NO INHERIT {}", p.to_sql())),
                        format!("Stop {} inheriting from {}", noun(r), p.to_sql()),
                    );
                }
                AlterOp::Persistence => {
                    step.sql(
                        sql::alter_table(
                            &name,
                            if des.unlogged {
                                "SET UNLOGGED"
                            } else {
                                "SET LOGGED"
                            },
                        ),
                        format!("Set persistence of {}", noun(r)),
                    );
                }
                AlterOp::StorageParams => {
                    for action in
                        sql::storage_param_actions(&cur.storage_params, &des.storage_params)
                    {
                        step.sql(
                            sql::alter_table(&name, &action),
                            format!("Set storage parameters of {}", noun(r)),
                        );
                    }
                }
                _ => {}
            }
        }
        if !step.statements.is_empty() {
            self.push(step);
        }
        let rls = ops.contains(&AlterOp::RowSecurity);
        let force = ops.contains(&AlterOp::ForceRowSecurity);
        if rls || force {
            let mut step = Step::new(r, Phase::Create, rank::RLS, " rls");
            if rls {
                let (action, verb) = if des.rls_enabled {
                    ("ENABLE ROW LEVEL SECURITY", "Enable")
                } else {
                    ("DISABLE ROW LEVEL SECURITY", "Disable")
                };
                step.sql(
                    sql::alter_table(&name, action),
                    format!("{verb} row level security on {}", noun(r)),
                );
            }
            if force {
                let (action, verb) = if des.rls_forced {
                    ("FORCE ROW LEVEL SECURITY", "Force")
                } else {
                    ("NO FORCE ROW LEVEL SECURITY", "Stop forcing")
                };
                step.sql(
                    sql::alter_table(&name, action),
                    format!("{verb} row level security on {}", noun(r)),
                );
            }
            self.push(step);
        }
        if ops.contains(&AlterOp::ReplicaIdentity) {
            self.replica_identity_step(r, &des.replica_identity);
        }
    }

    /// One step with every statement of a column change, in an order PostgreSQL accepts:
    /// expression, identity removal, default removal, type, nullability, identity, default,
    /// storage.
    fn alter_column(&mut self, r: &ObjectRef, cur: &Column, des: &Column, ops: &[AlterOp]) {
        let table = relation_of(r);
        let has = |op: &AlterOp| ops.contains(op);
        let default_rebuilt = self.default_rebuilt.contains(r);
        let type_change = has(&AlterOp::Type);
        let mut step = Step::new(r, Phase::Create, rank::COLUMN, " alter");
        let comment = format!("Alter {}", noun(r));
        let alter = |step: &mut Step, action: String| {
            step.sql(
                sql::alter_column(&table, &cur.name, &action),
                comment.clone(),
            );
        };
        if has(&AlterOp::Generated) {
            match &des.generated {
                Some(g) => alter(&mut step, format!("SET EXPRESSION AS ({})", g.expr)),
                None => alter(&mut step, "DROP EXPRESSION".into()),
            }
        }
        let identity = has(&AlterOp::Identity);
        if identity && cur.identity.is_some() && des.identity.is_none() {
            alter(&mut step, "DROP IDENTITY".into());
        }
        let touch_default = !default_rebuilt && (has(&AlterOp::Default) || type_change);
        if touch_default && cur.default.is_some() && (type_change || des.default.is_none()) {
            alter(&mut step, "DROP DEFAULT".into());
        }
        if type_change {
            alter(&mut step, sql::column_type_action(des));
        }
        if has(&AlterOp::NotNull) {
            if des.not_null {
                alter(&mut step, "SET NOT NULL".into());
            } else {
                alter(&mut step, "DROP NOT NULL".into());
                if let Some(t) = self.cur.table(&table)
                    && let Some(pk) = &t.primary_key
                    && pk.columns.contains(&cur.name)
                {
                    step.after_removal_of.push(ObjectRef::new(
                        ObjectKind::PrimaryKey,
                        [&table.schema, &table.name, &pk.name],
                    ));
                }
            }
        }
        if identity && let Some(d) = &des.identity {
            for action in identity_actions(cur.identity.as_ref(), d) {
                alter(&mut step, action);
            }
        }
        if touch_default
            && let Some(default) = &des.default
            && (type_change || has(&AlterOp::Default))
        {
            alter(&mut step, format!("SET DEFAULT {default}"));
        }
        if has(&AlterOp::Storage) {
            let storage = des.storage.map_or_else(
                || default_storage(&des.data_type, self.opts.major),
                Storage::as_sql,
            );
            alter(&mut step, format!("SET STORAGE {storage}"));
        }
        if has(&AlterOp::Compression) {
            let method = des.compression.as_deref().unwrap_or("default");
            alter(&mut step, format!("SET COMPRESSION {method}"));
        }
        if step.statements.is_empty() {
            return;
        }
        let blocking = type_change || (has(&AlterOp::Generated) && des.generated.is_some());
        if blocking {
            step.removes.push(r.clone());
            step.creates.push(r.clone());
        }
        step.releases.push(r.clone());
        if default_rebuilt && type_change {
            step.after_removal_of.push(super::rebuild::default_ref(r));
        }
        step.requires
            .push(r2(ObjectKind::Table, &table.schema, &table.name));
        step.requires.extend(type_refs(self.des, &des.data_type));
        step.requires.extend(self.des.dependencies_of(r).cloned());
        step.hints.extend(des.default.iter().cloned());
        self.push(step);
    }

    fn alter_policy(&mut self, r: &ObjectRef, cur: &Policy, des: &Policy) {
        let table = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::POLICY, " alter");
        step.sql(
            sql::alter_policy(&table, cur, des),
            format!("Alter {}", noun(r)),
        );
        step.releases.push(r.clone());
        step.creates.push(r.clone());
        step.requires.extend(
            des.roles
                .iter()
                .filter(|x| x.as_str() != crate::model::PUBLIC)
                .map(|x| ObjectRef::new(ObjectKind::Role, [x])),
        );
        step.requires.extend(self.des.dependencies_of(r).cloned());
        step.hints.extend(des.using.iter().cloned());
        step.hints.extend(des.check.iter().cloned());
        self.push(step);
    }

    fn alter_trigger(&mut self, r: &ObjectRef, des: &Trigger, ops: &[AlterOp]) {
        let rel = relation_of(r);
        let replace = ops.contains(&AlterOp::Replace);
        let enabled = ops.contains(&AlterOp::Enabled);
        if !replace && !enabled {
            return;
        }
        let mut step = Step::new(r, Phase::Create, rank::TRIGGER, " alter");
        if replace {
            step.sql(
                sql::create_trigger(&rel, des, true),
                format!("Replace {}", noun(r)),
            );
            step.releases.push(r.clone());
            step.creates.push(r.clone());
            step.requires
                .extend(functions_named(self.des, &des.function));
            step.requires.extend(self.des.dependencies_of(r).cloned());
            step.hints.extend(des.when.iter().cloned());
        }
        // `CREATE OR REPLACE TRIGGER` resets the firing mode.
        if enabled || (replace && des.enabled != TriggerEnabled::Origin) {
            step.sql(
                sql::trigger_enabled(&rel, &des.name, des.enabled),
                format!("Set firing of {}", noun(r)),
            );
        }
        self.push(step);
    }

    fn alter_view(&mut self, r: &ObjectRef, cur: &View, des: &View, ops: &[AlterOp]) {
        let name = relation_of(r);
        if ops.contains(&AlterOp::Replace) {
            let mut step = Step::new(r, Phase::Create, rank::VIEW, " replace");
            step.sql(
                sql::create_view(&name, des, true),
                format!("Replace {}", noun(r)),
            );
            step.releases.push(r.clone());
            step.creates.push(r.clone());
            step.creates.extend(
                des.columns.iter().map(|c| {
                    ObjectRef::new(ObjectKind::Column, [&name.schema, &name.name, &c.name])
                }),
            );
            step.requires.extend(self.des.dependencies_of(r).cloned());
            step.hints.push(des.query.clone());
            self.push(step);
        } else if ops.contains(&AlterOp::ViewOptions) {
            let mut step = Step::new(r, Phase::Create, rank::VIEW, " options");
            let actions =
                sql::storage_param_actions(&sql::view_options(cur), &sql::view_options(des));
            for action in actions {
                step.sql(
                    sql::alter_view(&name, &action),
                    format!("Set options of {}", noun(r)),
                );
            }
            self.push(step);
        }
    }

    fn alter_materialized_view(
        &mut self,
        r: &ObjectRef,
        cur: &MaterializedView,
        des: &MaterializedView,
        ops: &[AlterOp],
    ) {
        let name = relation_of(r);
        let mut step = Step::new(r, Phase::Create, rank::MATERIALIZED_VIEW, " alter");
        if ops.contains(&AlterOp::StorageParams) {
            for action in sql::storage_param_actions(&cur.storage_params, &des.storage_params) {
                step.sql(
                    sql::alter_materialized_view(&name, &action),
                    format!("Set storage parameters of {}", noun(r)),
                );
            }
        }
        if ops.contains(&AlterOp::Populate) {
            step.sql(
                sql::refresh_materialized_view(&name, des.with_data),
                format!("Refresh {}", noun(r)),
            );
        }
        if !step.statements.is_empty() {
            self.push(step);
        }
    }

    fn alter_routine(&mut self, r: &ObjectRef, cur: &Function, des: &Function, ops: &[AlterOp]) {
        let schema = &r.path[0];
        if ops.contains(&AlterOp::Replace) {
            let mut step = Step::new(r, Phase::Create, rank::FUNCTION, " replace");
            step.sql(
                sql::create_routine(schema, des, true),
                format!("Replace {}", noun(r)),
            );
            step.releases.push(r.clone());
            step.creates.push(r.clone());
            step.requires.extend(self.routine_requires(r, des));
            step.hints.extend(Self::routine_hints(des));
            self.push(step);
        } else if ops.contains(&AlterOp::Attributes)
            && let Some(statement) = sql::alter_routine(schema, cur, des)
        {
            let mut step = Step::new(r, Phase::Create, rank::FUNCTION, " attributes");
            step.sql(statement, format!("Alter {}", noun(r)));
            self.push(step);
        }
    }

    fn alter_sequence(&mut self, r: &ObjectRef, cur: &Sequence, des: &Sequence, ops: &[AlterOp]) {
        if ops.contains(&AlterOp::SequenceOptions) {
            let mut step = Step::new(r, Phase::Create, rank::SEQUENCE, " alter");
            step.sql(
                sql::alter_sequence(&r.path[0], des),
                format!("Alter {}", noun(r)),
            );
            self.push(step);
        }
        if ops.contains(&AlterOp::OwnedBy) {
            self.owned_by_step(r, des);
            if let (Some(step), Some(o)) = (self.steps.last_mut(), &cur.owned_by) {
                step.before_removal_of.push(ObjectRef::new(
                    ObjectKind::Column,
                    [&o.schema, &o.table, &o.column],
                ));
            }
        }
    }

    fn alter_composite(
        &mut self,
        r: &ObjectRef,
        _cur: &Composite,
        des: &Composite,
        ops: &[AlterOp],
    ) {
        let schema = &r.path[0];
        let mut step = Step::new(r, Phase::Create, rank::COMPOSITE, " alter");
        for op in ops {
            match op {
                AlterOp::DropAttribute(a) => {
                    step.sql(
                        sql::drop_attribute(schema, &des.name, a),
                        format!("Drop attribute {} of {}", sql::ident(a), noun(r)),
                    );
                }
                AlterOp::AlterAttribute(a) | AlterOp::AddAttribute(a) => {
                    let Some(f) = des.fields.iter().find(|f| &f.name == a) else {
                        continue;
                    };
                    let statement = if matches!(op, AlterOp::AddAttribute(_)) {
                        sql::add_attribute(schema, &des.name, f)
                    } else {
                        sql::alter_attribute_type(schema, &des.name, f)
                    };
                    step.sql(
                        statement,
                        format!("Alter attribute {} of {}", sql::ident(a), noun(r)),
                    );
                    step.requires.extend(type_refs(self.des, &f.data_type));
                }
                _ => {}
            }
        }
        if !step.statements.is_empty() {
            step.releases.push(r.clone());
            step.creates.push(r.clone());
            self.push(step);
        }
    }

    fn alter_domain(&mut self, r: &ObjectRef, _cur: &Domain, des: &Domain, ops: &[AlterOp]) {
        let schema = &r.path[0];
        let mut step = Step::new(r, Phase::Create, rank::DOMAIN, " alter");
        if ops.contains(&AlterOp::Default) && !self.default_rebuilt.contains(r) {
            let action = match &des.default {
                Some(d) => format!("SET DEFAULT {d}"),
                None => "DROP DEFAULT".to_owned(),
            };
            step.sql(
                sql::alter_domain(schema, &des.name, &action),
                format!("Alter {}", noun(r)),
            );
        }
        if ops.contains(&AlterOp::NotNull) {
            let action = if des.not_null {
                "SET NOT NULL"
            } else {
                "DROP NOT NULL"
            };
            step.sql(
                sql::alter_domain(schema, &des.name, action),
                format!("Alter {}", noun(r)),
            );
        }
        if !step.statements.is_empty() {
            step.releases.push(r.clone());
            step.requires.extend(self.des.dependencies_of(r).cloned());
            step.hints.extend(des.default.iter().cloned());
            self.push(step);
        }
    }
}

/// `ALTER COLUMN` actions that turn identity `cur` (if any) into `des`.
fn identity_actions(cur: Option<&Identity>, des: &Identity) -> Vec<String> {
    let Some(cur) = cur else {
        return vec![format!("ADD {}", sql::identity_clause(des))];
    };
    let mut out = Vec::new();
    if cur.generation != des.generation {
        out.push(format!(
            "SET GENERATED {}",
            sql::identity_generation(des.generation)
        ));
    }
    if cur.sequence != des.sequence {
        out.push(sql::identity_set_options(&des.sequence));
    }
    out
}

/// Statements that give `member` the membership `des` in `group`, starting from `cur`.
fn membership_statements(
    group: &str,
    member: &str,
    cur: Option<Membership>,
    des: Membership,
    major: u32,
) -> Vec<String> {
    let Some(cur) = cur else {
        return vec![sql::grant_membership(group, member, &des, major)];
    };
    let mut out = Vec::new();
    let options: &[(&str, bool, bool)] = if major >= 16 {
        &[
            ("ADMIN", cur.admin, des.admin),
            ("INHERIT", cur.inherit, des.inherit),
            ("SET", cur.set, des.set),
        ]
    } else {
        &[("ADMIN", cur.admin, des.admin)]
    };
    let granted: Vec<String> = options
        .iter()
        .filter(|(_, c, d)| !c && *d)
        .map(|(o, _, _)| {
            if major >= 16 {
                format!("{o} TRUE")
            } else {
                format!("{o} OPTION")
            }
        })
        .collect();
    if !granted.is_empty() {
        out.push(format!(
            "GRANT {} TO {} WITH {}",
            sql::ident(group),
            sql::ident(member),
            granted.join(", ")
        ));
    }
    for (o, c, d) in options {
        if *c && !d {
            out.push(sql::revoke_membership(group, member, Some(o)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{IdentityGeneration, SequenceOptions};

    #[test]
    fn identities() {
        let d = Identity {
            generation: IdentityGeneration::ByDefault,
            sequence: SequenceOptions::defaults_for("integer"),
        };
        assert_eq!(
            identity_actions(None, &d),
            vec![
                "ADD GENERATED BY DEFAULT AS IDENTITY (START WITH 1 INCREMENT BY 1 MINVALUE 1 \
                 MAXVALUE 2147483647 CACHE 1 NO CYCLE)"
                    .to_owned()
            ]
        );
        let mut c = d.clone();
        c.generation = IdentityGeneration::Always;
        c.sequence.increment = 2;
        assert_eq!(
            identity_actions(Some(&c), &d),
            vec![
                "SET GENERATED BY DEFAULT".to_owned(),
                "SET START WITH 1 SET INCREMENT BY 1 SET MINVALUE 1 SET MAXVALUE 2147483647 SET \
                 CACHE 1 SET NO CYCLE"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn memberships() {
        let m = |admin, inherit, set| Membership {
            admin,
            inherit,
            set,
        };
        assert_eq!(
            membership_statements(
                "g",
                "u",
                Some(m(false, true, true)),
                m(true, false, true),
                16
            ),
            vec![
                "GRANT \"g\" TO \"u\" WITH ADMIN TRUE".to_owned(),
                "REVOKE INHERIT OPTION FOR \"g\" FROM \"u\"".to_owned()
            ]
        );
        assert_eq!(
            membership_statements(
                "g",
                "u",
                Some(m(true, true, true)),
                m(false, true, true),
                15
            ),
            vec!["REVOKE ADMIN OPTION FOR \"g\" FROM \"u\"".to_owned()]
        );
        assert_eq!(default_storage("numeric(10,2)", 15), "MAIN");
        assert_eq!(default_storage("text[]", 15), "EXTENDED");
        assert_eq!(default_storage("integer", 15), "PLAIN");
        assert_eq!(default_storage("integer", 16), "DEFAULT");
    }
}
