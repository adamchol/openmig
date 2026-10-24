//! Addressing model objects by [`ObjectRef`].
//!
//! Paths: databases are `[database]`; tables, views, materialized views and dictionaries are
//! `[database, name]`; columns, indexes, projections, constraints and row policies are
//! `[database, table, name]`; roles, users and functions are `[name]`. Grants have no object of
//! their own (see [`Grant::object_ref`](super::Grant::object_ref)).

use super::{
    Column, Constraint, Database, Dictionary, Function, Index, MaterializedView, Model, Projection,
    Role, RowPolicy, RowPolicyId, Table, User, View,
};
use openmig_core::{ObjectKind, ObjectRef};

/// A borrowed model object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Object<'a> {
    Database(&'a Database),
    Table(&'a Table),
    /// A column of a table or of a materialized view.
    Column(&'a Column),
    Index(&'a Index),
    Projection(&'a Projection),
    Constraint(&'a Constraint),
    View(&'a View),
    MaterializedView(&'a MaterializedView),
    Dictionary(&'a Dictionary),
    Function(&'a Function),
    Role(&'a Role),
    User(&'a User),
    RowPolicy(&'a RowPolicy),
}

fn r2(kind: ObjectKind, a: &str, b: &str) -> ObjectRef {
    ObjectRef::new(kind, [a, b])
}

fn r3(kind: ObjectKind, a: &str, b: &str, c: &str) -> ObjectRef {
    ObjectRef::new(kind, [a, b, c])
}

impl Model {
    /// The object `object` refers to.
    pub fn get(&self, object: &ObjectRef) -> Option<Object<'_>> {
        use ObjectKind as K;
        let p: Vec<&str> = object.path.iter().map(String::as_str).collect();
        match (object.kind, p.as_slice()) {
            (K::Role, [n]) => self.roles.get(*n).map(Object::Role),
            (K::User, [n]) => self.users.get(*n).map(Object::User),
            (K::Function, [n]) => self.functions.get(*n).map(Object::Function),
            (K::RowPolicy, [d, t, n]) => self
                .row_policies
                .get(&RowPolicyId {
                    database: (*d).to_owned(),
                    table: (*t).to_owned(),
                    name: (*n).to_owned(),
                })
                .map(Object::RowPolicy),
            (K::Database, [d]) => self.databases.get(*d).map(Object::Database),
            (kind, [d, rest @ ..]) => database_object(self.databases.get(*d)?, kind, rest),
            _ => None,
        }
    }

    pub fn contains(&self, object: &ObjectRef) -> bool {
        self.get(object).is_some()
    }

    /// Every object with its reference, in a deterministic order: functions, roles, users,
    /// databases with their contents, row policies.
    pub fn objects(&self) -> Vec<(ObjectRef, Object<'_>)> {
        use ObjectKind as K;
        let mut out = Vec::new();
        for (name, f) in &self.functions {
            out.push((
                ObjectRef::new(K::Function, [name.as_str()]),
                Object::Function(f),
            ));
        }
        for (name, r) in &self.roles {
            out.push((ObjectRef::new(K::Role, [name.as_str()]), Object::Role(r)));
        }
        for (name, u) in &self.users {
            out.push((ObjectRef::new(K::User, [name.as_str()]), Object::User(u)));
        }
        for (d, db) in &self.databases {
            out.push((
                ObjectRef::new(K::Database, [d.as_str()]),
                Object::Database(db),
            ));
            for (t, table) in &db.tables {
                out.push((r2(K::Table, d, t), Object::Table(table)));
                for c in &table.columns {
                    out.push((r3(K::Column, d, t, &c.name), Object::Column(c)));
                }
                for (n, i) in &table.indexes {
                    out.push((r3(K::Index, d, t, n), Object::Index(i)));
                }
                for (n, p) in &table.projections {
                    out.push((r3(K::Projection, d, t, n), Object::Projection(p)));
                }
                for (n, c) in &table.constraints {
                    out.push((r3(K::Constraint, d, t, n), Object::Constraint(c)));
                }
            }
            for (v, view) in &db.views {
                out.push((r2(K::View, d, v), Object::View(view)));
            }
            for (v, mv) in &db.materialized_views {
                out.push((r2(K::MaterializedView, d, v), Object::MaterializedView(mv)));
                for c in &mv.columns {
                    out.push((r3(K::Column, d, v, &c.name), Object::Column(c)));
                }
            }
            for (n, dict) in &db.dictionaries {
                out.push((r2(K::Dictionary, d, n), Object::Dictionary(dict)));
            }
        }
        for (id, policy) in &self.row_policies {
            out.push((id.object_ref(), Object::RowPolicy(policy)));
        }
        out
    }
}

fn database_object<'a>(db: &'a Database, kind: ObjectKind, rest: &[&str]) -> Option<Object<'a>> {
    use ObjectKind as K;
    match (kind, rest) {
        (K::Table, [t]) => db.tables.get(*t).map(Object::Table),
        (K::View, [v]) => db.views.get(*v).map(Object::View),
        (K::MaterializedView, [v]) => db.materialized_views.get(*v).map(Object::MaterializedView),
        (K::Dictionary, [n]) => db.dictionaries.get(*n).map(Object::Dictionary),
        (K::Column, [t, c]) => match db.tables.get(*t) {
            Some(table) => table.column(c).map(Object::Column),
            None => db
                .materialized_views
                .get(*t)?
                .columns
                .iter()
                .find(|col| col.name == *c)
                .map(Object::Column),
        },
        (K::Index, [t, n]) => db.tables.get(*t)?.indexes.get(*n).map(Object::Index),
        (K::Projection, [t, n]) => db
            .tables
            .get(*t)?
            .projections
            .get(*n)
            .map(Object::Projection),
        (K::Constraint, [t, n]) => db
            .tables
            .get(*t)?
            .constraints
            .get(*n)
            .map(Object::Constraint),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, ConstraintKind, Grantee, RoleSet};

    fn model() -> Model {
        let mut table = Table::new("events", "MergeTree");
        table.columns.push(Column::new("id", "UInt64"));
        table.indexes.insert(
            "idx".into(),
            Index {
                expression: "id".into(),
                index_type: "minmax".into(),
                granularity: 1,
            },
        );
        table.constraints.insert(
            "positive".into(),
            Constraint {
                kind: ConstraintKind::Check,
                expression: "id > 0".into(),
            },
        );
        let mut db = Database::new("app", "Atomic");
        db.tables.insert("events".into(), table);
        db.materialized_views.insert(
            "mv".into(),
            MaterializedView {
                name: "mv".into(),
                columns: vec![Column::new("n", "UInt64")],
                ..MaterializedView::default()
            },
        );
        let mut model = Model::default();
        model.databases.insert("app".into(), db);
        model.roles.insert(
            "web".into(),
            Role {
                name: "web".into(),
                storage: "local_directory".into(),
            },
        );
        model.row_policies.insert(
            RowPolicyId {
                database: "app".into(),
                table: "events".into(),
                name: "p".into(),
            },
            RowPolicy {
                filter: Some("id = 1".into()),
                to: RoleSet {
                    names: vec![Grantee::Role("web".into()).name().to_owned()],
                    ..RoleSet::default()
                },
                ..RowPolicy::default()
            },
        );
        model
    }

    #[test]
    fn lookup_and_iteration_agree() {
        let model = model();
        let objects = model.objects();
        let refs: Vec<String> = objects.iter().map(|(r, _)| r.to_string()).collect();
        assert_eq!(
            refs,
            [
                "role web",
                "database app",
                "table app.events",
                "column app.events.id",
                "index app.events.idx",
                "constraint app.events.positive",
                "materialized_view app.mv",
                "column app.mv.n",
                "row_policy app.events.p",
            ]
        );
        for (r, object) in &objects {
            assert_eq!(model.get(r), Some(*object), "{r}");
        }
        assert!(!model.contains(&ObjectRef::new(ObjectKind::Table, ["app", "nope"])));
        assert!(!model.contains(&ObjectRef::new(ObjectKind::Column, ["app", "nope", "id"])));
    }
}
