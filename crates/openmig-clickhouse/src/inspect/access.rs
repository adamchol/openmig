//! Roles, users, grants, role memberships and row policies.

use super::{Snapshot, list, rows};
use crate::model::{
    Grant, GrantLevel, Grantee, Model, Role, RoleGrant, RoleSet, RowPolicy, RowPolicyId, User,
};
use crate::privileges::PrivilegeCatalog;
use crate::session::ChSession;
use openmig_core::Error;
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};

/// Storage of access entities defined in configuration files; they cannot be changed with SQL.
const CONFIG_STORAGE: &str = "users_xml";

/// The built-in user every server has.
const DEFAULT_USER: &str = "default";

#[derive(Deserialize)]
pub(crate) struct RoleRow {
    name: String,
    storage: String,
}

#[derive(Deserialize)]
pub(crate) struct UserRow {
    name: String,
    storage: String,
    host_ip: Vec<String>,
    host_names: Vec<String>,
    host_names_regexp: Vec<String>,
    host_names_like: Vec<String>,
    #[serde(deserialize_with = "flag")]
    default_roles_all: bool,
    default_roles_list: Vec<String>,
    default_roles_except: Vec<String>,
    #[serde(deserialize_with = "flag")]
    grantees_any: bool,
    grantees_list: Vec<String>,
    grantees_except: Vec<String>,
    #[serde(default)]
    default_database: String,
}

#[derive(Deserialize)]
pub(crate) struct GrantRow {
    user_name: Option<String>,
    role_name: Option<String>,
    access_type: String,
    #[serde(default)]
    access_object: Option<String>,
    database: Option<String>,
    table: Option<String>,
    column: Option<String>,
    #[serde(deserialize_with = "flag")]
    is_partial_revoke: bool,
    #[serde(deserialize_with = "flag")]
    grant_option: bool,
}

#[derive(Deserialize)]
pub(crate) struct RoleGrantRow {
    user_name: Option<String>,
    role_name: Option<String>,
    granted_role_name: String,
    #[serde(deserialize_with = "flag")]
    with_admin_option: bool,
}

#[derive(Deserialize)]
pub(crate) struct RowPolicyRow {
    short_name: String,
    database: String,
    table: String,
    storage: String,
    select_filter: Option<String>,
    #[serde(deserialize_with = "flag")]
    is_restrictive: bool,
    #[serde(deserialize_with = "flag")]
    apply_to_all: bool,
    apply_to_list: Vec<String>,
    apply_to_except: Vec<String>,
}

pub(crate) struct AccessRows {
    roles: Vec<RoleRow>,
    users: Vec<UserRow>,
    grants: Vec<GrantRow>,
    role_grants: Vec<RoleGrantRow>,
    row_policies: Vec<RowPolicyRow>,
}

/// A `UInt8` or `Bool` column.
fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Number(u64),
    }
    Ok(match Flag::deserialize(deserializer)? {
        Flag::Bool(b) => b,
        Flag::Number(n) => n != 0,
    })
}

pub(super) async fn read(session: &ChSession, databases: &[String]) -> Result<AccessRows, Error> {
    let roles = rows(
        session,
        "SELECT name, storage FROM system.roles ORDER BY name",
    )
    .await?;
    let users = rows(
        session,
        "SELECT name, storage, host_ip, host_names, host_names_regexp, host_names_like, \
         default_roles_all, default_roles_list, default_roles_except, grantees_any, \
         grantees_list, grantees_except, default_database FROM system.users ORDER BY name",
    )
    .await?;
    let grants = rows(session, "SELECT * FROM system.grants").await?;
    let role_grants = rows(
        session,
        "SELECT user_name, role_name, granted_role_name, with_admin_option FROM system.role_grants",
    )
    .await?;
    let row_policies = if databases.is_empty() {
        Vec::new()
    } else {
        rows(
            session,
            &format!(
                "SELECT short_name, database, table, storage, select_filter, is_restrictive, \
                 apply_to_all, apply_to_list, apply_to_except FROM system.row_policies \
                 WHERE storage != {} AND database IN ({}) ORDER BY database, table, short_name",
                list(&[CONFIG_STORAGE.to_owned()]),
                list(databases)
            ),
        )
        .await?
    };
    Ok(AccessRows {
        roles,
        users,
        grants,
        role_grants,
        row_policies,
    })
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

fn role_set(all: bool, names: &[String], except: &[String]) -> RoleSet {
    RoleSet {
        all,
        names: sorted(names.to_vec()),
        except: sorted(except.to_vec()),
    }
}

pub(super) fn assemble(model: &mut Model, snapshot: &Snapshot, catalog: &PrivilegeCatalog) {
    let rows = &snapshot.access;
    // Grantees whose grants are managed with SQL: not defined in configuration files, not the
    // connecting user and not `default`.
    let mut managed: BTreeSet<Grantee> = BTreeSet::new();
    for role in &rows.roles {
        if role.storage != CONFIG_STORAGE {
            managed.insert(Grantee::Role(role.name.clone()));
            model.roles.insert(
                role.name.clone(),
                Role {
                    name: role.name.clone(),
                    storage: role.storage.clone(),
                },
            );
        }
    }
    for user in &rows.users {
        if user.storage == CONFIG_STORAGE
            || user.name == snapshot.current_user
            || user.name == DEFAULT_USER
        {
            continue;
        }
        managed.insert(Grantee::User(user.name.clone()));
        model.users.insert(
            user.name.clone(),
            User {
                name: user.name.clone(),
                storage: user.storage.clone(),
                host_ip: sorted(user.host_ip.clone()),
                host_names: sorted(user.host_names.clone()),
                host_names_regexp: sorted(user.host_names_regexp.clone()),
                host_names_like: sorted(user.host_names_like.clone()),
                default_roles: role_set(
                    user.default_roles_all,
                    &user.default_roles_list,
                    &user.default_roles_except,
                ),
                default_database: Some(user.default_database.clone()).filter(|d| !d.is_empty()),
                grantees: role_set(
                    user.grantees_any,
                    &user.grantees_list,
                    &user.grantees_except,
                ),
            },
        );
    }
    model.grants = grants(&rows.grants, catalog, &managed);
    for row in &rows.role_grants {
        if let Some(grantee) =
            grantee_of(&row.user_name, &row.role_name).filter(|g| managed.contains(g))
        {
            model.role_grants.push(RoleGrant {
                grantee,
                role: row.granted_role_name.clone(),
                admin_option: row.with_admin_option,
            });
        }
    }
    for row in &rows.row_policies {
        model.row_policies.insert(
            RowPolicyId {
                database: row.database.clone(),
                table: row.table.clone(),
                name: row.short_name.clone(),
            },
            RowPolicy {
                filter: row
                    .select_filter
                    .as_deref()
                    .map(crate::ddl::normalize_expression),
                restrictive: row.is_restrictive,
                to: role_set(row.apply_to_all, &row.apply_to_list, &row.apply_to_except),
                storage: row.storage.clone(),
            },
        );
    }
}

fn grantee_of(user: &Option<String>, role: &Option<String>) -> Option<Grantee> {
    match (user, role) {
        (Some(u), _) => Some(Grantee::User(u.clone())),
        (None, Some(r)) => Some(Grantee::Role(r.clone())),
        (None, None) => None,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct GrantKey {
    grantee: Grantee,
    database: Option<String>,
    table: Option<String>,
    parameter: Option<String>,
    partial_revoke: bool,
    grant_option: bool,
}

/// Group `system.grants` rows into canonical [`Grant`]s: per grantee, object, revoke flag and
/// grant option, one grant for the whole object and one per distinct set of column privileges
/// (columns with equal privileges grouped), privileges collapsed with `catalog`. Rows of grantees
/// outside `managed` are skipped.
fn grants(
    rows: &[GrantRow],
    catalog: &PrivilegeCatalog,
    managed: &BTreeSet<Grantee>,
) -> Vec<Grant> {
    #[derive(Default)]
    struct Group<'a> {
        object: BTreeSet<&'a str>,
        columns: BTreeMap<&'a str, BTreeSet<&'a str>>,
    }
    let mut groups: BTreeMap<GrantKey, Group<'_>> = BTreeMap::new();
    for row in rows {
        let Some(grantee) =
            grantee_of(&row.user_name, &row.role_name).filter(|g| managed.contains(g))
        else {
            continue;
        };
        let key = GrantKey {
            grantee,
            database: row.database.clone(),
            table: row.table.clone(),
            parameter: row.access_object.clone().filter(|o| !o.is_empty()),
            partial_revoke: row.is_partial_revoke,
            grant_option: row.grant_option,
        };
        let group = groups.entry(key).or_default();
        match &row.column {
            Some(column) => {
                group
                    .columns
                    .entry(column)
                    .or_default()
                    .insert(&row.access_type);
            }
            None => {
                group.object.insert(&row.access_type);
            }
        }
    }

    let mut out = Vec::new();
    for (key, group) in groups {
        let grant = |privileges: BTreeSet<String>, columns: Vec<String>| Grant {
            grantee: key.grantee.clone(),
            database: key.database.clone(),
            table: key.table.clone(),
            parameter: key.parameter.clone(),
            columns,
            partial_revoke: key.partial_revoke,
            grant_option: key.grant_option,
            privileges,
        };
        let level = grant(BTreeSet::new(), Vec::new()).level();
        if !group.object.is_empty() {
            out.push(grant(catalog.collapse(&group.object, level), Vec::new()));
        }
        let mut by_privileges: BTreeMap<BTreeSet<String>, Vec<String>> = BTreeMap::new();
        for (column, privileges) in group.columns {
            by_privileges
                .entry(catalog.collapse(&privileges, GrantLevel::Column))
                .or_default()
                .push(column.to_owned());
        }
        for (privileges, columns) in by_privileges {
            out.push(grant(privileges, columns));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privileges::PrivilegeRow;

    fn catalog() -> PrivilegeCatalog {
        PrivilegeCatalog::new(
            include_str!("../../tests/fixtures/privileges-26.5.jsonl")
                .lines()
                .map(|line| serde_json::from_str::<PrivilegeRow>(line).unwrap()),
        )
    }

    fn row(
        role: &str,
        access: &str,
        db: Option<&str>,
        table: Option<&str>,
        column: Option<&str>,
    ) -> GrantRow {
        GrantRow {
            user_name: None,
            role_name: Some(role.into()),
            access_type: access.into(),
            access_object: Some(String::new()),
            database: db.map(Into::into),
            table: table.map(Into::into),
            column: column.map(Into::into),
            is_partial_revoke: false,
            grant_option: false,
        }
    }

    #[test]
    fn groups_columns_and_collapses() {
        let rows = vec![
            row("r", "SELECT", Some("app"), Some("t"), Some("id")),
            row("r", "SELECT", Some("app"), Some("t"), Some("name")),
            row("r", "INSERT", Some("app"), Some("t"), Some("id")),
            row("r", "INSERT", Some("app"), Some("t"), None),
            row("r", "CREATE TABLE", Some("app"), None, None),
            row("r", "CREATE VIEW", Some("app"), None, None),
            row("r", "CREATE DICTIONARY", Some("app"), None, None),
            row("r", "CREATE DATABASE", Some("app"), None, None),
            row("ignored", "SELECT", None, None, None),
        ];
        let managed = BTreeSet::from([Grantee::Role("r".into())]);
        let mut grants = grants(&rows, &catalog(), &managed);
        grants.sort();
        let summary: Vec<String> = grants
            .iter()
            .map(|g| {
                format!(
                    "{} {:?} {}",
                    g.target_sql(),
                    g.columns,
                    g.privileges.iter().cloned().collect::<Vec<_>>().join(",")
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                "`app`.* [] CREATE",
                "`app`.`t` [] INSERT",
                "`app`.`t` [\"id\"] INSERT,SELECT",
                "`app`.`t` [\"name\"] SELECT",
            ]
        );
    }

    #[test]
    fn flags_accept_numbers_and_booleans() {
        let a: RoleGrantRow = serde_json::from_str(
            r#"{"user_name":"u","role_name":null,"granted_role_name":"r","with_admin_option":1}"#,
        )
        .unwrap();
        assert!(a.with_admin_option);
        let b: RoleGrantRow = serde_json::from_str(
            r#"{"user_name":null,"role_name":"x","granted_role_name":"r","with_admin_option":false}"#,
        )
        .unwrap();
        assert!(!b.with_admin_option);
    }
}
