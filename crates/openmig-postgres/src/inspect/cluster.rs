use super::{Catalog, SystemCatalog, code};
use crate::model::{
    Database, EventTrigger, Extension, Membership, QualifiedName, Role, TriggerEnabled,
};
use openmig_core::{ObjectKind, ObjectRef, Scope};
use std::collections::BTreeMap;
use tokio_postgres::Row;

/// Extensions installed into a managed schema.
pub(super) fn extensions(db: &mut Database, cat: &mut Catalog, rows: &[Row], scope: &Scope) {
    for row in rows {
        let name: String = row.get("name");
        let schema: String = row.get("schema");
        cat.objects.insert(
            (SystemCatalog::Extension, row.get("oid")),
            ObjectRef::new(ObjectKind::Extension, [&name]),
        );
        if !scope.schemas.includes(&schema) {
            continue;
        }
        db.extensions.insert(
            name.clone(),
            Extension {
                name,
                schema,
                version: row.get("version"),
                comment: row.get("comment"),
            },
        );
    }
}

pub(super) fn event_triggers(db: &mut Database, cat: &mut Catalog, rows: &[Row]) {
    for row in rows {
        let name: String = row.get("name");
        let mut tags: Vec<String> = row.get("tags");
        tags.sort();
        let enabled = match code(row, "enabled") {
            'D' => TriggerEnabled::Disabled,
            'R' => TriggerEnabled::Replica,
            'A' => TriggerEnabled::Always,
            _ => TriggerEnabled::Origin,
        };
        cat.objects.insert(
            (SystemCatalog::EventTrigger, row.get("oid")),
            ObjectRef::new(ObjectKind::EventTrigger, [&name]),
        );
        db.event_triggers.insert(
            name.clone(),
            EventTrigger {
                name,
                event: row.get("event"),
                tags,
                function: QualifiedName::new(
                    row.get::<_, String>("func_schema"),
                    row.get::<_, String>("func_name"),
                ),
                enabled,
                comment: row.get("comment"),
                owner: row.get("owner"),
            },
        );
    }
}

/// Roles and their memberships. Rows are empty unless roles are managed.
pub(super) fn roles(db: &mut Database, roles: &[Row], memberships: &[Row]) {
    let mut names = BTreeMap::new();
    for row in roles {
        let name: String = row.get("name");
        names.insert(row.get::<_, u32>("oid"), name.clone());
        db.roles.insert(
            name.clone(),
            Role {
                name,
                login: row.get("login"),
                superuser: row.get("superuser"),
                createdb: row.get("createdb"),
                createrole: row.get("createrole"),
                inherit: row.get("inherit"),
                replication: row.get("replication"),
                bypassrls: row.get("bypassrls"),
                connection_limit: row.get("connection_limit"),
                valid_until: row.get("valid_until"),
                member_of: BTreeMap::new(),
                comment: row.get("comment"),
            },
        );
    }
    for row in memberships {
        let Some(member) = names.get(&row.get::<_, u32>("member")) else {
            continue;
        };
        let role = db
            .roles
            .get_mut(member)
            .expect("member is an inspected role");
        let admin: bool = row.get("admin");
        let inherit: bool = row
            .get::<_, Option<bool>>("inherit")
            .unwrap_or(role.inherit);
        let set: bool = row.get::<_, Option<bool>>("set").unwrap_or(true);
        // Since PostgreSQL 16 one membership can be granted by several grantors; merge them.
        role.member_of
            .entry(row.get("role"))
            .and_modify(|m: &mut Membership| {
                m.admin |= admin;
                m.inherit |= inherit;
                m.set |= set;
            })
            .or_insert(Membership {
                admin,
                inherit,
                set,
            });
    }
}
