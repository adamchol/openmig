//! Applying a [`Scope`] to a model.

use crate::model::{Acl, Database, PUBLIC, Schema};
use openmig_core::{ExcludeSet, ObjectKind, Scope};

fn excluded(ex: &ExcludeSet, kind: ObjectKind, path: &[&str]) -> bool {
    if ex.is_empty() {
        return false;
    }
    let path: Vec<String> = path.iter().map(|s| (*s).to_owned()).collect();
    ex.matches(kind, &path)
}

/// Whether a role is a built-in role (`pg_*`), which is never managed.
pub fn is_builtin_role(name: &str) -> bool {
    name.starts_with("pg_")
}

/// Whether privileges granted to `grantee` are compared: `PUBLIC`, roles declared in the schema,
/// and every role when roles are managed, except excluded roles.
pub fn is_managed_grantee(grantee: &str, scope: &Scope) -> bool {
    if grantee == PUBLIC {
        return true;
    }
    if excluded(&scope.exclude, ObjectKind::Role, &[grantee]) {
        return false;
    }
    scope.manage_roles || scope.declared_roles.iter().any(|r| r == grantee)
}

/// Remove what is outside `scope` from a model, exactly as inspection leaves it out: schemas not
/// in the schema filter (and extensions installed into them), excluded objects of every kind
/// (with everything inside them), roles unless they are managed, built-in roles, memberships in
/// and grants to excluded roles, rows of tables that are gone, and dependencies on removed
/// objects.
pub fn restrict(db: &mut Database, scope: &Scope) {
    let ex = &scope.exclude;
    db.schemas
        .retain(|n, _| scope.schemas.includes(n) && !excluded(ex, ObjectKind::Schema, &[n]));
    for schema in db.schemas.values_mut() {
        restrict_schema(schema, ex);
    }
    db.extensions.retain(|n, e| {
        scope.schemas.includes(&e.schema) && !excluded(ex, ObjectKind::Extension, &[n])
    });
    if scope.manage_roles {
        db.roles
            .retain(|n, _| !is_builtin_role(n) && !excluded(ex, ObjectKind::Role, &[n]));
        for role in db.roles.values_mut() {
            role.member_of
                .retain(|g, _| !excluded(ex, ObjectKind::Role, &[g]));
        }
    } else {
        db.roles.clear();
    }
    db.event_triggers
        .retain(|n, _| !excluded(ex, ObjectKind::EventTrigger, &[n]));
    if !ex.is_empty() {
        for_each_acl(db, &mut |acl| {
            acl.retain(|item| {
                item.grantee == PUBLIC || !excluded(ex, ObjectKind::Role, &[&item.grantee])
            });
        });
    }
    let gone: Vec<_> = db
        .data
        .keys()
        .filter(|t| db.table(t).is_none() || excluded(ex, ObjectKind::Data, &[&t.schema, &t.name]))
        .cloned()
        .collect();
    for t in gone {
        db.data.remove(&t);
    }
    let dependencies = std::mem::take(&mut db.dependencies);
    db.dependencies = dependencies
        .into_iter()
        .filter(|d| db.contains(&d.from) && db.contains(&d.to))
        .collect();
}

fn restrict_schema(s: &mut Schema, ex: &ExcludeSet) {
    if ex.is_empty() {
        return;
    }
    use ObjectKind as K;
    let n = s.name.clone();
    let n = n.as_str();
    s.tables.retain(|t, _| !excluded(ex, K::Table, &[n, t]));
    for t in s.tables.values_mut() {
        let tn = t.name.clone();
        let tn = tn.as_str();
        t.columns
            .retain(|c| !excluded(ex, K::Column, &[n, tn, &c.name]));
        if t.primary_key
            .as_ref()
            .is_some_and(|pk| excluded(ex, K::PrimaryKey, &[n, tn, &pk.name]))
        {
            t.primary_key = None;
        }
        t.foreign_keys
            .retain(|c, _| !excluded(ex, K::ForeignKey, &[n, tn, c]));
        t.uniques
            .retain(|c, _| !excluded(ex, K::Unique, &[n, tn, c]));
        t.checks.retain(|c, _| !excluded(ex, K::Check, &[n, tn, c]));
        t.exclusions
            .retain(|c, _| !excluded(ex, K::Exclusion, &[n, tn, c]));
        t.indexes
            .retain(|i, _| !excluded(ex, K::Index, &[n, tn, i]));
        t.policies
            .retain(|p, _| !excluded(ex, K::Policy, &[n, tn, p]));
        t.triggers
            .retain(|g, _| !excluded(ex, K::Trigger, &[n, tn, g]));
    }
    s.views.retain(|v, _| !excluded(ex, K::View, &[n, v]));
    for v in s.views.values_mut() {
        let vn = v.name.clone();
        v.triggers
            .retain(|g, _| !excluded(ex, K::Trigger, &[n, &vn, g]));
    }
    s.materialized_views
        .retain(|m, _| !excluded(ex, K::MaterializedView, &[n, m]));
    for m in s.materialized_views.values_mut() {
        let mn = m.name.clone();
        m.indexes
            .retain(|i, _| !excluded(ex, K::Index, &[n, &mn, i]));
    }
    s.functions
        .retain(|f, _| !excluded(ex, K::Function, &[n, f]));
    s.procedures
        .retain(|f, _| !excluded(ex, K::Procedure, &[n, f]));
    s.sequences
        .retain(|q, _| !excluded(ex, K::Sequence, &[n, q]));
    s.enums.retain(|e, _| !excluded(ex, K::Enum, &[n, e]));
    s.composites
        .retain(|c, _| !excluded(ex, K::Composite, &[n, c]));
    s.domains.retain(|d, _| !excluded(ex, K::Domain, &[n, d]));
    for d in s.domains.values_mut() {
        let dn = d.name.clone();
        d.checks
            .retain(|c, _| !excluded(ex, K::Check, &[n, &dn, c]));
    }
}

/// Visit every ACL in the model (objects and columns).
pub fn for_each_acl(db: &mut Database, f: &mut impl FnMut(&mut Acl)) {
    for s in db.schemas.values_mut() {
        f(&mut s.acl);
        for t in s.tables.values_mut() {
            f(&mut t.acl);
            for c in &mut t.columns {
                f(&mut c.acl);
            }
        }
        for v in s.views.values_mut() {
            f(&mut v.acl);
            for c in &mut v.columns {
                f(&mut c.acl);
            }
        }
        for m in s.materialized_views.values_mut() {
            f(&mut m.acl);
            for c in &mut m.columns {
                f(&mut c.acl);
            }
        }
        for x in s.functions.values_mut().chain(s.procedures.values_mut()) {
            f(&mut x.acl);
        }
        for x in s.sequences.values_mut() {
            f(&mut x.acl);
        }
        for x in s.enums.values_mut() {
            f(&mut x.acl);
        }
        for x in s.composites.values_mut() {
            f(&mut x.acl);
        }
        for x in s.domains.values_mut() {
            f(&mut x.acl);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AclItem, Column, Privilege, Role, Table};

    fn scope(exclude: &[&str], manage_roles: bool) -> Scope {
        Scope {
            exclude: ExcludeSet::new(exclude).unwrap(),
            manage_roles,
            ..Scope::default()
        }
    }

    fn model() -> Database {
        let mut t = Table::new("users");
        t.columns.push(Column::new("id", "bigint"));
        t.columns.push(Column::new("secret", "text"));
        t.acl.insert(AclItem::new("web", Privilege::Select));
        t.acl.insert(AclItem::new("cnpg_pooler", Privilege::Select));
        let mut app = Schema::new("app");
        app.tables.insert("users".into(), t);
        app.tables.insert("legacy_x".into(), Table::new("legacy_x"));
        let mut db = Database::default();
        db.schemas.insert("app".into(), app);
        db.schemas.insert("other".into(), Schema::new("other"));
        for r in ["web", "cnpg_pooler", "pg_monitor"] {
            db.roles.insert(r.into(), Role::new(r));
        }
        db
    }

    #[test]
    fn excludes_and_roles() {
        let mut db = model();
        restrict(
            &mut db,
            &scope(
                &[
                    "app.legacy_*",
                    "column:app.users.secret",
                    "role:cnpg_*",
                    "other",
                ],
                true,
            ),
        );
        assert_eq!(db.schemas.keys().collect::<Vec<_>>(), ["app"]);
        let app = &db.schemas["app"];
        assert_eq!(app.tables.keys().collect::<Vec<_>>(), ["users"]);
        let users = &app.tables["users"];
        assert_eq!(users.columns.len(), 1);
        assert_eq!(
            users.acl,
            Acl::from([AclItem::new("web", Privilege::Select)])
        );
        assert_eq!(db.roles.keys().collect::<Vec<_>>(), ["web"]);

        let mut db = model();
        restrict(&mut db, &scope(&[], false));
        assert!(db.roles.is_empty());
    }

    #[test]
    fn managed_grantees() {
        let mut s = scope(&["role:cnpg_*"], false);
        s.declared_roles = vec!["web".into()];
        assert!(is_managed_grantee(PUBLIC, &s));
        assert!(is_managed_grantee("web", &s));
        assert!(!is_managed_grantee("other", &s));
        s.manage_roles = true;
        assert!(is_managed_grantee("other", &s));
        assert!(!is_managed_grantee("cnpg_x", &s));
    }
}
