//! Addressing model objects by [`ObjectRef`].
//!
//! Paths: schema-level objects are `[schema, name]` (functions and procedures
//! `[schema, "name(arg types)"]`); objects nested in a relation or type are
//! `[schema, relation, name]` (columns, constraints, indexes, policies, triggers, composite fields,
//! domain checks); cluster-wide objects are `[name]`; table data is `[schema, table]`.

use super::{
    Check, Column, Composite, CompositeField, Database, Domain, DomainCheck, Enum, EventTrigger,
    Exclusion, Extension, ForeignKey, Function, Index, MaterializedView, Policy, PrimaryKey,
    QualifiedName, Role, Schema, Sequence, Table, TableData, Trigger, Unique, View, ViewColumn,
};
use openmig_core::{ObjectKind, ObjectRef};

/// A borrowed model object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Object<'a> {
    Schema(&'a Schema),
    Extension(&'a Extension),
    Role(&'a Role),
    EventTrigger(&'a EventTrigger),
    Table(&'a Table),
    Column(&'a Column),
    ViewColumn(&'a ViewColumn),
    CompositeField(&'a CompositeField),
    PrimaryKey(&'a PrimaryKey),
    ForeignKey(&'a ForeignKey),
    Unique(&'a Unique),
    Check(&'a Check),
    DomainCheck(&'a DomainCheck),
    Exclusion(&'a Exclusion),
    Index(&'a Index),
    Policy(&'a Policy),
    Trigger(&'a Trigger),
    View(&'a View),
    MaterializedView(&'a MaterializedView),
    Function(&'a Function),
    Procedure(&'a Function),
    Enum(&'a Enum),
    Composite(&'a Composite),
    Domain(&'a Domain),
    Sequence(&'a Sequence),
    Data(&'a TableData),
}

fn r2(kind: ObjectKind, a: &str, b: &str) -> ObjectRef {
    ObjectRef::new(kind, [a, b])
}

fn r3(kind: ObjectKind, a: &str, b: &str, c: &str) -> ObjectRef {
    ObjectRef::new(kind, [a, b, c])
}

impl Database {
    /// The object `object` refers to.
    pub fn get(&self, object: &ObjectRef) -> Option<Object<'_>> {
        use ObjectKind as K;
        let p: Vec<&str> = object.path.iter().map(String::as_str).collect();
        match (object.kind, p.as_slice()) {
            (K::Schema, [s]) => self.schemas.get(*s).map(Object::Schema),
            (K::Extension, [n]) => self.extensions.get(*n).map(Object::Extension),
            (K::Role, [n]) => self.roles.get(*n).map(Object::Role),
            (K::EventTrigger, [n]) => self.event_triggers.get(*n).map(Object::EventTrigger),
            (K::Data, [s, t]) => self.data.get(&QualifiedName::new(*s, *t)).map(Object::Data),
            (kind, [s, rest @ ..]) => {
                let schema = self.schemas.get(*s)?;
                schema_object(schema, kind, rest)
            }
            _ => None,
        }
    }

    pub fn contains(&self, object: &ObjectRef) -> bool {
        self.get(object).is_some()
    }

    /// Every object with its reference, in a deterministic order: extensions, roles, schemas with
    /// their contents, event triggers, table data.
    pub fn objects(&self) -> Vec<(ObjectRef, Object<'_>)> {
        let mut out = Vec::new();
        for e in self.extensions.values() {
            out.push((
                ObjectRef::new(ObjectKind::Extension, [&e.name]),
                Object::Extension(e),
            ));
        }
        for r in self.roles.values() {
            out.push((ObjectRef::new(ObjectKind::Role, [&r.name]), Object::Role(r)));
        }
        for s in self.schemas.values() {
            schema_objects(s, &mut out);
        }
        for e in self.event_triggers.values() {
            out.push((
                ObjectRef::new(ObjectKind::EventTrigger, [&e.name]),
                Object::EventTrigger(e),
            ));
        }
        for (name, d) in &self.data {
            out.push((
                r2(ObjectKind::Data, &name.schema, &name.name),
                Object::Data(d),
            ));
        }
        out
    }
}

fn schema_object<'a>(schema: &'a Schema, kind: ObjectKind, rest: &[&str]) -> Option<Object<'a>> {
    use ObjectKind as K;
    match (kind, rest) {
        (K::Table, [t]) => schema.tables.get(*t).map(Object::Table),
        (K::View, [v]) => schema.views.get(*v).map(Object::View),
        (K::MaterializedView, [m]) => schema
            .materialized_views
            .get(*m)
            .map(Object::MaterializedView),
        (K::Function, [f]) => schema.functions.get(*f).map(Object::Function),
        (K::Procedure, [f]) => schema.procedures.get(*f).map(Object::Procedure),
        (K::Sequence, [n]) => schema.sequences.get(*n).map(Object::Sequence),
        (K::Enum, [n]) => schema.enums.get(*n).map(Object::Enum),
        (K::Composite, [n]) => schema.composites.get(*n).map(Object::Composite),
        (K::Domain, [n]) => schema.domains.get(*n).map(Object::Domain),
        (K::Column, [rel, col]) => {
            if let Some(t) = schema.tables.get(*rel) {
                return t.column(col).map(Object::Column);
            }
            let view_column = |cols: &'a [ViewColumn]| {
                cols.iter().find(|c| c.name == *col).map(Object::ViewColumn)
            };
            if let Some(v) = schema.views.get(*rel) {
                return view_column(&v.columns);
            }
            if let Some(m) = schema.materialized_views.get(*rel) {
                return view_column(&m.columns);
            }
            schema
                .composites
                .get(*rel)?
                .fields
                .iter()
                .find(|f| f.name == *col)
                .map(Object::CompositeField)
        }
        (K::PrimaryKey, [t, n]) => schema
            .tables
            .get(*t)?
            .primary_key
            .as_ref()
            .filter(|pk| pk.name == *n)
            .map(Object::PrimaryKey),
        (K::ForeignKey, [t, n]) => schema
            .tables
            .get(*t)?
            .foreign_keys
            .get(*n)
            .map(Object::ForeignKey),
        (K::Unique, [t, n]) => schema.tables.get(*t)?.uniques.get(*n).map(Object::Unique),
        (K::Exclusion, [t, n]) => schema
            .tables
            .get(*t)?
            .exclusions
            .get(*n)
            .map(Object::Exclusion),
        (K::Check, [t, n]) => match schema.tables.get(*t) {
            Some(table) => table.checks.get(*n).map(Object::Check),
            None => schema
                .domains
                .get(*t)?
                .checks
                .get(*n)
                .map(Object::DomainCheck),
        },
        (K::Index, [t, n]) => match schema.tables.get(*t) {
            Some(table) => table.indexes.get(*n).map(Object::Index),
            None => schema
                .materialized_views
                .get(*t)?
                .indexes
                .get(*n)
                .map(Object::Index),
        },
        (K::Policy, [t, n]) => schema.tables.get(*t)?.policies.get(*n).map(Object::Policy),
        (K::Trigger, [t, n]) => match schema.tables.get(*t) {
            Some(table) => table.triggers.get(*n).map(Object::Trigger),
            None => schema.views.get(*t)?.triggers.get(*n).map(Object::Trigger),
        },
        _ => None,
    }
}

fn schema_objects<'a>(s: &'a Schema, out: &mut Vec<(ObjectRef, Object<'a>)>) {
    use ObjectKind as K;
    let n = s.name.as_str();
    out.push((ObjectRef::new(K::Schema, [n]), Object::Schema(s)));
    for e in s.enums.values() {
        out.push((r2(K::Enum, n, &e.name), Object::Enum(e)));
    }
    for c in s.composites.values() {
        out.push((r2(K::Composite, n, &c.name), Object::Composite(c)));
        for f in &c.fields {
            out.push((
                r3(K::Column, n, &c.name, &f.name),
                Object::CompositeField(f),
            ));
        }
    }
    for d in s.domains.values() {
        out.push((r2(K::Domain, n, &d.name), Object::Domain(d)));
        for c in d.checks.values() {
            out.push((r3(K::Check, n, &d.name, &c.name), Object::DomainCheck(c)));
        }
    }
    for q in s.sequences.values() {
        out.push((r2(K::Sequence, n, &q.name), Object::Sequence(q)));
    }
    for t in s.tables.values() {
        let tn = t.name.as_str();
        out.push((r2(K::Table, n, tn), Object::Table(t)));
        for c in &t.columns {
            out.push((r3(K::Column, n, tn, &c.name), Object::Column(c)));
        }
        if let Some(pk) = &t.primary_key {
            out.push((r3(K::PrimaryKey, n, tn, &pk.name), Object::PrimaryKey(pk)));
        }
        for c in t.foreign_keys.values() {
            out.push((r3(K::ForeignKey, n, tn, &c.name), Object::ForeignKey(c)));
        }
        for c in t.uniques.values() {
            out.push((r3(K::Unique, n, tn, &c.name), Object::Unique(c)));
        }
        for c in t.checks.values() {
            out.push((r3(K::Check, n, tn, &c.name), Object::Check(c)));
        }
        for c in t.exclusions.values() {
            out.push((r3(K::Exclusion, n, tn, &c.name), Object::Exclusion(c)));
        }
        for i in t.indexes.values() {
            out.push((r3(K::Index, n, tn, &i.name), Object::Index(i)));
        }
        for p in t.policies.values() {
            out.push((r3(K::Policy, n, tn, &p.name), Object::Policy(p)));
        }
        for g in t.triggers.values() {
            out.push((r3(K::Trigger, n, tn, &g.name), Object::Trigger(g)));
        }
    }
    for v in s.views.values() {
        out.push((r2(K::View, n, &v.name), Object::View(v)));
        for c in &v.columns {
            out.push((r3(K::Column, n, &v.name, &c.name), Object::ViewColumn(c)));
        }
        for g in v.triggers.values() {
            out.push((r3(K::Trigger, n, &v.name, &g.name), Object::Trigger(g)));
        }
    }
    for m in s.materialized_views.values() {
        out.push((
            r2(K::MaterializedView, n, &m.name),
            Object::MaterializedView(m),
        ));
        for c in &m.columns {
            out.push((r3(K::Column, n, &m.name, &c.name), Object::ViewColumn(c)));
        }
        for i in m.indexes.values() {
            out.push((r3(K::Index, n, &m.name, &i.name), Object::Index(i)));
        }
    }
    for (sig, f) in &s.functions {
        out.push((r2(K::Function, n, sig), Object::Function(f)));
    }
    for (sig, f) in &s.procedures {
        out.push((r2(K::Procedure, n, sig), Object::Procedure(f)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, PrimaryKey, Schema, Table};

    #[test]
    fn get_and_objects_agree() {
        let mut t = Table::new("users");
        t.columns.push(Column::new("id", "bigint"));
        t.primary_key = Some(PrimaryKey {
            name: "users_pkey".into(),
            columns: vec!["id".into()],
            ..PrimaryKey::default()
        });
        let mut s = Schema::new("app");
        s.tables.insert("users".into(), t);
        let mut db = Database::default();
        db.schemas.insert("app".into(), s);

        let objects = db.objects();
        assert_eq!(objects.len(), 4);
        for (r, o) in &objects {
            assert_eq!(db.get(r), Some(*o), "{r}");
        }
        assert!(db.contains(&ObjectRef::new(ObjectKind::Column, ["app", "users", "id"])));
        assert!(!db.contains(&ObjectRef::new(ObjectKind::Column, ["app", "users", "x"])));
        assert!(!db.contains(&ObjectRef::new(
            ObjectKind::PrimaryKey,
            ["app", "users", "x"]
        )));
    }
}
