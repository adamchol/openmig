use openmig_core::sql::{Dialect, quote_ident};
use openmig_core::{ObjectKind, ObjectRef};
use std::collections::BTreeSet;
use std::fmt;

/// A role stored in a SQL-managed user directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    /// Name of the user directory holding the role (`system.roles.storage`): `local_directory`,
    /// `replicated`.
    pub storage: String,
}

/// A user stored in a SQL-managed user directory. Authentication is never part of the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct User {
    pub name: String,
    /// See [`Role::storage`].
    pub storage: String,
    /// `HOST IP` patterns (`::/0` means any host). Sorted, like the other host lists.
    pub host_ip: Vec<String>,
    /// `HOST NAME` patterns.
    pub host_names: Vec<String>,
    /// `HOST REGEXP` patterns.
    pub host_names_regexp: Vec<String>,
    /// `HOST LIKE` patterns.
    pub host_names_like: Vec<String>,
    /// `DEFAULT ROLE`.
    pub default_roles: RoleSet,
    pub default_database: Option<String>,
    /// `GRANTEES`: who this user may grant its privileges to.
    pub grantees: RoleSet,
}

/// A set of roles or users as in `DEFAULT ROLE`, `GRANTEES` and `TO` of row policies:
/// `ALL`, `ALL EXCEPT a, b`, `a, b` or `NONE` (empty).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoleSet {
    pub all: bool,
    /// Sorted.
    pub names: Vec<String>,
    /// Sorted; only meaningful with `all`.
    pub except: Vec<String>,
}

impl RoleSet {
    pub fn is_none(&self) -> bool {
        !self.all && self.names.is_empty()
    }
}

/// Who a privilege or role is granted to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grantee {
    Role(String),
    User(String),
}

impl Grantee {
    pub fn name(&self) -> &str {
        match self {
            Grantee::Role(name) | Grantee::User(name) => name,
        }
    }

    pub fn object_ref(&self) -> ObjectRef {
        match self {
            Grantee::Role(name) => ObjectRef::new(ObjectKind::Role, [name.as_str()]),
            Grantee::User(name) => ObjectRef::new(ObjectKind::User, [name.as_str()]),
        }
    }
}

impl fmt::Display for Grantee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Grantee::Role(name) => write!(f, "role {name}"),
            Grantee::User(name) => write!(f, "user {name}"),
        }
    }
}

/// The kind of object a grant applies to; determines which privileges can be granted on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GrantLevel {
    /// `ON *.*`.
    Global,
    /// `ON db.*`.
    Database,
    /// `ON db.table`.
    Table,
    /// `(columns) ON db.table`.
    Column,
    /// `ON <parameter>` for privileges on named collections, users, table engines and sources
    /// (`READ ON S3`, `TABLE ENGINE ON MergeTree`).
    Parameter,
}

/// Privileges granted to one grantee on one object.
///
/// Field order is the sort order of [`Model::grants`](super::Model::grants). The inspected
/// form has one entry per grantee, object, columns, grant option and revoke flag, with columns
/// that have the same privileges grouped together and privileges collapsed with the server's
/// privilege hierarchy (`ALL` instead of every privilege it implies).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    pub grantee: Grantee,
    /// `None` for every database (`*.*`).
    pub database: Option<String>,
    /// `None` for every table of the database (`db.*`).
    pub table: Option<String>,
    /// Object of a privilege granted on a named collection, user, table engine or source;
    /// `database` and `table` are `None` then.
    pub parameter: Option<String>,
    /// Column-level privileges apply to these columns (sorted); empty for the whole table.
    pub columns: Vec<String>,
    /// A partial revoke: `REVOKE SELECT ON system.* FROM r` after `GRANT SELECT ON *.* TO r`.
    pub partial_revoke: bool,
    pub grant_option: bool,
    /// Canonical privilege names as `system.privileges` lists them: `SELECT`, `ALTER UPDATE`,
    /// `dictGet`, `ALL`.
    pub privileges: BTreeSet<String>,
}

impl Default for Grantee {
    fn default() -> Self {
        Grantee::Role(String::new())
    }
}

impl Grant {
    pub fn level(&self) -> GrantLevel {
        if self.parameter.is_some() {
            GrantLevel::Parameter
        } else if !self.columns.is_empty() {
            GrantLevel::Column
        } else if self.table.is_some() {
            GrantLevel::Table
        } else if self.database.is_some() {
            GrantLevel::Database
        } else {
            GrantLevel::Global
        }
    }

    /// What the grant is on, as written after `ON`: `*.*`, `` `db`.* ``, `` `db`.`t` ``,
    /// `` `S3` ``.
    pub fn target_sql(&self) -> String {
        let q = |s: &str| quote_ident(s, Dialect::ClickHouse);
        match (&self.parameter, &self.database, &self.table) {
            (Some(parameter), _, _) => q(parameter),
            (None, None, _) => "*.*".to_owned(),
            (None, Some(db), None) => format!("{}.*", q(db)),
            (None, Some(db), Some(table)) => format!("{}.{}", q(db), q(table)),
        }
    }

    /// `[grantee, database or "*", table or "*"]`, or `[grantee, parameter]`, for excludes and
    /// messages (`grant:web.app.*`).
    pub fn object_ref(&self) -> ObjectRef {
        let grantee = self.grantee.name();
        match &self.parameter {
            Some(parameter) => ObjectRef::new(ObjectKind::Grant, [grantee, parameter.as_str()]),
            None => ObjectRef::new(
                ObjectKind::Grant,
                [
                    grantee,
                    self.database.as_deref().unwrap_or("*"),
                    self.table.as_deref().unwrap_or("*"),
                ],
            ),
        }
    }
}

/// `GRANT role TO grantee [WITH ADMIN OPTION]`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoleGrant {
    pub grantee: Grantee,
    pub role: String,
    pub admin_option: bool,
}

/// Identity of a row policy: `name ON database.table`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowPolicyId {
    pub database: String,
    pub table: String,
    pub name: String,
}

impl RowPolicyId {
    pub fn object_ref(&self) -> ObjectRef {
        ObjectRef::new(
            ObjectKind::RowPolicy,
            [
                self.database.as_str(),
                self.table.as_str(),
                self.name.as_str(),
            ],
        )
    }
}

/// `CREATE ROW POLICY name ON db.table [AS RESTRICTIVE] FOR SELECT USING filter TO roles`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowPolicy {
    /// The `USING` condition as the server prints it.
    pub filter: Option<String>,
    pub restrictive: bool,
    pub to: RoleSet,
    /// See [`Role::storage`].
    pub storage: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(database: Option<&str>, table: Option<&str>) -> Grant {
        Grant {
            grantee: Grantee::Role("web".into()),
            database: database.map(Into::into),
            table: table.map(Into::into),
            privileges: BTreeSet::from(["SELECT".to_owned()]),
            ..Grant::default()
        }
    }

    #[test]
    fn grant_targets() {
        let global = grant(None, None);
        assert_eq!(global.level(), GrantLevel::Global);
        assert_eq!(global.target_sql(), "*.*");
        assert_eq!(global.object_ref().dotted(), "web.*.*");

        let database = grant(Some("app"), None);
        assert_eq!(database.level(), GrantLevel::Database);
        assert_eq!(database.target_sql(), "`app`.*");

        let mut column = grant(Some("app"), Some("t"));
        assert_eq!(column.level(), GrantLevel::Table);
        assert_eq!(column.target_sql(), "`app`.`t`");
        column.columns = vec!["id".into()];
        assert_eq!(column.level(), GrantLevel::Column);
        assert_eq!(column.object_ref().dotted(), "web.app.t");

        let parameter = Grant {
            parameter: Some("S3".into()),
            ..grant(None, None)
        };
        assert_eq!(parameter.level(), GrantLevel::Parameter);
        assert_eq!(parameter.target_sql(), "`S3`");
        assert_eq!(parameter.object_ref().dotted(), "web.S3");
    }

    #[test]
    fn grants_sort_by_grantee_then_object() {
        let mut grants = [
            grant(Some("b"), None),
            grant(None, None),
            Grant {
                grantee: Grantee::Role("api".into()),
                ..grant(Some("z"), None)
            },
            grant(Some("a"), Some("t")),
        ];
        grants.sort();
        let targets: Vec<String> = grants
            .iter()
            .map(|g| format!("{} {}", g.grantee.name(), g.target_sql()))
            .collect();
        assert_eq!(
            targets,
            ["api `z`.*", "web *.*", "web `a`.`t`", "web `b`.*"]
        );
    }
}
