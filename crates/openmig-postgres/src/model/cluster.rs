use super::{QualifiedName, TriggerEnabled};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    /// Schema the extension's objects were installed into.
    pub schema: String,
    pub version: String,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub createdb: bool,
    pub createrole: bool,
    pub inherit: bool,
    pub replication: bool,
    pub bypassrls: bool,
    /// `-1` for no limit.
    pub connection_limit: i32,
    /// `VALID UNTIL` as PostgreSQL prints it in UTC with ISO dates (`2027-01-01 00:00:00+00`,
    /// `infinity`).
    pub valid_until: Option<String>,
    /// Roles this role is a member of.
    pub member_of: BTreeMap<String, Membership>,
    pub comment: Option<String>,
}

impl Role {
    /// A role with PostgreSQL's `CREATE ROLE` defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            login: false,
            superuser: false,
            createdb: false,
            createrole: false,
            inherit: true,
            replication: false,
            bypassrls: false,
            connection_limit: -1,
            valid_until: None,
            member_of: BTreeMap::new(),
            comment: None,
        }
    }
}

/// Options of a role membership. Before PostgreSQL 16 `inherit` follows the member's `INHERIT`
/// attribute and `set` is always true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Membership {
    pub admin: bool,
    pub inherit: bool,
    pub set: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventTrigger {
    pub name: String,
    /// `ddl_command_start`, `ddl_command_end`, `sql_drop`, `table_rewrite`, `login`.
    pub event: String,
    /// `WHEN TAG IN (...)`, sorted.
    pub tags: Vec<String>,
    pub function: QualifiedName,
    pub enabled: TriggerEnabled,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
}
