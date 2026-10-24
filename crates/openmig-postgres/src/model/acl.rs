use openmig_core::ObjectKind;
use std::collections::BTreeSet;
use std::fmt;

/// The grantee name used for `PUBLIC` in ACLs and policy role lists.
pub const PUBLIC: &str = "PUBLIC";

/// The privileges granted on an object to roles other than its owner.
///
/// Inspected ACLs are effective: a NULL catalog ACL is replaced by PostgreSQL's built-in default
/// for the kind of object. Entries for the owner, the bootstrap superuser, the connecting user and
/// excluded roles are left out.
pub type Acl = BTreeSet<AclItem>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AclItem {
    /// Role name, or [`PUBLIC`].
    pub grantee: String,
    pub privilege: Privilege,
    /// `WITH GRANT OPTION`.
    pub grantable: bool,
}

impl AclItem {
    pub fn new(grantee: impl Into<String>, privilege: Privilege) -> Self {
        Self {
            grantee: grantee.into(),
            privilege,
            grantable: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    /// PostgreSQL 17+.
    Maintain,
    Execute,
    Usage,
    Create,
    Connect,
    Temporary,
    /// Configuration parameters (PostgreSQL 15+).
    Set,
    /// Configuration parameters (PostgreSQL 15+).
    AlterSystem,
}

impl Privilege {
    pub const ALL: &'static [Privilege] = &[
        Privilege::Select,
        Privilege::Insert,
        Privilege::Update,
        Privilege::Delete,
        Privilege::Truncate,
        Privilege::References,
        Privilege::Trigger,
        Privilege::Maintain,
        Privilege::Execute,
        Privilege::Usage,
        Privilege::Create,
        Privilege::Connect,
        Privilege::Temporary,
        Privilege::Set,
        Privilege::AlterSystem,
    ];

    /// The keyword used in `GRANT` and printed by `aclexplode`.
    pub fn as_sql(self) -> &'static str {
        match self {
            Privilege::Select => "SELECT",
            Privilege::Insert => "INSERT",
            Privilege::Update => "UPDATE",
            Privilege::Delete => "DELETE",
            Privilege::Truncate => "TRUNCATE",
            Privilege::References => "REFERENCES",
            Privilege::Trigger => "TRIGGER",
            Privilege::Maintain => "MAINTAIN",
            Privilege::Execute => "EXECUTE",
            Privilege::Usage => "USAGE",
            Privilege::Create => "CREATE",
            Privilege::Connect => "CONNECT",
            Privilege::Temporary => "TEMPORARY",
            Privilege::Set => "SET",
            Privilege::AlterSystem => "ALTER SYSTEM",
        }
    }

    /// Parse a privilege keyword (case-insensitive; `TEMP` is accepted for `TEMPORARY`).
    pub fn parse(s: &str) -> Option<Self> {
        let upper = s.trim().to_ascii_uppercase();
        if upper == "TEMP" {
            return Some(Privilege::Temporary);
        }
        Self::ALL.iter().copied().find(|p| p.as_sql() == upper)
    }
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// PostgreSQL's built-in privileges for roles other than the owner, i.e. what an object of this
/// kind grants when its ACL was never changed: `EXECUTE` to `PUBLIC` for functions and procedures,
/// `USAGE` to `PUBLIC` for types and domains, `CONNECT` and `TEMPORARY` to `PUBLIC` for databases,
/// nothing for every other kind.
pub fn default_acl(kind: ObjectKind) -> Acl {
    let privileges: &[Privilege] = match kind {
        ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate => {
            &[Privilege::Execute]
        }
        ObjectKind::Enum | ObjectKind::Composite | ObjectKind::Domain => &[Privilege::Usage],
        ObjectKind::Database => &[Privilege::Connect, Privilege::Temporary],
        _ => &[],
    };
    privileges
        .iter()
        .map(|&p| AclItem::new(PUBLIC, p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_round_trip() {
        for p in Privilege::ALL {
            assert_eq!(Privilege::parse(p.as_sql()), Some(*p));
        }
        assert_eq!(Privilege::parse("temp"), Some(Privilege::Temporary));
        assert_eq!(Privilege::parse("bogus"), None);
    }

    #[test]
    fn defaults() {
        assert_eq!(
            default_acl(ObjectKind::Function),
            Acl::from([AclItem::new(PUBLIC, Privilege::Execute)])
        );
        assert_eq!(default_acl(ObjectKind::Database).len(), 2);
        assert!(default_acl(ObjectKind::Table).is_empty());
        assert!(default_acl(ObjectKind::Schema).is_empty());
        assert!(default_acl(ObjectKind::Sequence).is_empty());
    }
}
