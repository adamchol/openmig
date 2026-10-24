//! Removing out-of-scope and excluded objects from a model.

use crate::model::{Grant, Grantee, Model};
use openmig_core::{ObjectKind, Scope};

/// Keep only what `scope` manages:
///
/// * databases selected by `scope.schemas` and not excluded, and in them every object that is not
///   excluded (excludes match each object kind by its path, see [`crate::model::Object`]);
/// * functions that are not excluded;
/// * roles, users and role memberships only when `scope.manage_roles`;
/// * grants of managed grantees (every role and user with `manage_roles`, otherwise the roles
///   declared in the schema) on objects in scope;
/// * row policies on tables in scope;
/// * dependencies between remaining objects.
pub fn restrict(model: &mut Model, scope: &Scope) {
    let ex = &scope.exclude;
    let excluded = |kind: ObjectKind, path: &[&str]| {
        let path: Vec<String> = path.iter().map(|s| (*s).to_owned()).collect();
        ex.matches(kind, &path)
    };
    let database_in_scope =
        |name: &str| scope.schemas.includes(name) && !excluded(ObjectKind::Database, &[name]);

    model.databases.retain(|name, _| database_in_scope(name));
    for (d, db) in &mut model.databases {
        let d = d.as_str();
        db.tables.retain(|t, table| {
            if excluded(ObjectKind::Table, &[d, t]) {
                return false;
            }
            table
                .columns
                .retain(|c| !excluded(ObjectKind::Column, &[d, t, &c.name]));
            table
                .indexes
                .retain(|n, _| !excluded(ObjectKind::Index, &[d, t, n]));
            table
                .projections
                .retain(|n, _| !excluded(ObjectKind::Projection, &[d, t, n]));
            table
                .constraints
                .retain(|n, _| !excluded(ObjectKind::Constraint, &[d, t, n]));
            true
        });
        db.views.retain(|v, _| !excluded(ObjectKind::View, &[d, v]));
        db.materialized_views
            .retain(|v, _| !excluded(ObjectKind::MaterializedView, &[d, v]));
        db.dictionaries
            .retain(|n, _| !excluded(ObjectKind::Dictionary, &[d, n]));
    }
    model
        .functions
        .retain(|n, _| !excluded(ObjectKind::Function, &[n]));

    if scope.manage_roles {
        model.roles.retain(|n, _| !excluded(ObjectKind::Role, &[n]));
        model.users.retain(|n, _| !excluded(ObjectKind::User, &[n]));
    } else {
        model.roles.clear();
        model.users.clear();
        model.role_grants.clear();
    }
    let grantee_managed = |grantee: &Grantee| {
        let declared =
            scope.manage_roles || scope.declared_roles.iter().any(|r| r == grantee.name());
        declared && !ex.is_excluded(&grantee.object_ref())
    };
    model
        .role_grants
        .retain(|g| grantee_managed(&g.grantee) && !excluded(ObjectKind::Role, &[&g.role]));
    let databases: Vec<String> = model.databases.keys().cloned().collect();
    let object_in_scope = |grant: &Grant| match (&grant.database, &grant.table) {
        (None, _) => true,
        (Some(d), table) => {
            databases.contains(d)
                && table
                    .as_deref()
                    .is_none_or(|t| !excluded(ObjectKind::Table, &[d, t]))
        }
    };
    model.grants.retain(|g| {
        grantee_managed(&g.grantee) && object_in_scope(g) && !ex.is_excluded(&g.object_ref())
    });
    model.row_policies.retain(|id, _| {
        databases.contains(&id.database)
            && !excluded(ObjectKind::Table, &[&id.database, &id.table])
            && !ex.is_excluded(&id.object_ref())
    });

    let dependencies = std::mem::take(&mut model.dependencies);
    model.dependencies = dependencies
        .into_iter()
        .filter(|d| model.contains(&d.from) && model.contains(&d.to))
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Column, Database, Dependency, Function, MaterializedView, Role, RoleGrant, RowPolicy,
        RowPolicyId, Table, User,
    };
    use openmig_core::{ExcludeSet, ObjectRef, SchemaFilter};
    use std::collections::BTreeSet;

    fn model() -> Model {
        let mut model = Model::default();
        for name in ["app", "legacy", "other"] {
            let mut db = Database::new(name, "Atomic");
            let mut events = Table::new("events", "MergeTree");
            events.columns = vec![Column::new("id", "UInt64"), Column::new("secret", "String")];
            db.tables.insert("events".into(), events);
            db.tables
                .insert("tmp_x".into(), Table::new("tmp_x", "Memory"));
            db.materialized_views
                .insert("mv".into(), MaterializedView::default());
            model.databases.insert(name.into(), db);
        }
        model.functions.insert("f".into(), Function::default());
        model
            .functions
            .insert("debug_f".into(), Function::default());
        for name in ["web", "cnpg_x"] {
            model.roles.insert(name.into(), Role::default());
        }
        model.users.insert("app_user".into(), User::default());
        let grant = |grantee: Grantee, database: Option<&str>| Grant {
            grantee,
            database: database.map(Into::into),
            privileges: BTreeSet::from(["SELECT".to_owned()]),
            ..Grant::default()
        };
        model.grants = vec![
            grant(Grantee::Role("web".into()), Some("app")),
            grant(Grantee::Role("web".into()), Some("legacy")),
            grant(Grantee::Role("web".into()), None),
            grant(Grantee::Role("cnpg_x".into()), None),
            grant(Grantee::Role("undeclared".into()), None),
            grant(Grantee::User("app_user".into()), Some("app")),
        ];
        model.role_grants = vec![RoleGrant {
            grantee: Grantee::User("app_user".into()),
            role: "web".into(),
            admin_option: false,
        }];
        model.row_policies.insert(
            RowPolicyId {
                database: "legacy".into(),
                table: "events".into(),
                name: "p".into(),
            },
            RowPolicy::default(),
        );
        model.dependencies = vec![
            Dependency {
                from: ObjectRef::new(ObjectKind::MaterializedView, ["app", "mv"]),
                to: ObjectRef::new(ObjectKind::Table, ["app", "events"]),
                soft: false,
            },
            Dependency {
                from: ObjectRef::new(ObjectKind::MaterializedView, ["app", "mv"]),
                to: ObjectRef::new(ObjectKind::Table, ["app", "tmp_x"]),
                soft: false,
            },
        ];
        model
    }

    #[test]
    fn schemas_excludes_and_roles() {
        let mut m = model();
        let scope = Scope {
            schemas: SchemaFilter::Only(vec!["app".into(), "legacy".into()]),
            exclude: ExcludeSet::new([
                "legacy",
                "table:*.tmp_*",
                "column:app.events.secret",
                "function:debug_*",
                "role:cnpg_*",
            ])
            .unwrap(),
            manage_roles: true,
            ..Scope::default()
        };
        restrict(&mut m, &scope);
        assert_eq!(m.databases.keys().collect::<Vec<_>>(), ["app"]);
        let app = &m.databases["app"];
        assert_eq!(app.tables.keys().collect::<Vec<_>>(), ["events"]);
        assert_eq!(app.tables["events"].columns.len(), 1);
        assert_eq!(m.functions.keys().collect::<Vec<_>>(), ["f"]);
        assert_eq!(m.roles.keys().collect::<Vec<_>>(), ["web"]);
        let grants: Vec<String> = m
            .grants
            .iter()
            .map(|g| format!("{} {}", g.grantee.name(), g.target_sql()))
            .collect();
        assert_eq!(
            grants,
            [
                "web `app`.*",
                "web *.*",
                "undeclared *.*",
                "app_user `app`.*"
            ]
        );
        assert_eq!(m.role_grants.len(), 1);
        assert!(m.row_policies.is_empty());
        assert_eq!(m.dependencies.len(), 1);
    }

    #[test]
    fn unmanaged_roles_keep_declared_grants() {
        let mut m = model();
        let scope = Scope {
            declared_roles: vec!["web".into()],
            ..Scope::default()
        };
        restrict(&mut m, &scope);
        assert!(m.roles.is_empty() && m.users.is_empty() && m.role_grants.is_empty());
        assert_eq!(m.grants.len(), 3);
        assert!(m.grants.iter().all(|g| g.grantee.name() == "web"));
        assert_eq!(m.row_policies.len(), 1);
    }
}
