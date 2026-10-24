//! Keyword attributes (`on_delete = CASCADE`, `for = ROW`) mapped onto model types.

use crate::model::{
    ArgMode, FkAction, FkMatch, IdentityGeneration, NullsOrder, Parallel, PolicyCommand, Privilege,
    Storage, TriggerEnabled, TriggerEvent, TriggerLevel, ViewCheckOption, Volatility,
};
use openmig_core::DataSync;
use openmig_hcl::Keyword;

/// Keywords compare upper-cased, with spaces written as underscores (`SET NULL` = `SET_NULL`).
fn normalize(word: &str) -> String {
    word.trim().to_ascii_uppercase().replace(' ', "_")
}

macro_rules! keyword {
    ($ty:ty, $expected:literal, { $($word:literal => $value:expr),* $(,)? }) => {
        impl Keyword for $ty {
            const EXPECTED: &'static str = $expected;
            fn parse(word: &str) -> Option<Self> {
                match normalize(word).as_str() {
                    $($word => Some($value),)*
                    _ => None,
                }
            }
        }
    };
}

keyword!(FkAction, "NO_ACTION, RESTRICT, CASCADE, SET_NULL or SET_DEFAULT", {
    "NO_ACTION" => FkAction::NoAction,
    "RESTRICT" => FkAction::Restrict,
    "CASCADE" => FkAction::Cascade,
    "SET_NULL" => FkAction::SetNull,
    "SET_DEFAULT" => FkAction::SetDefault,
});

keyword!(FkMatch, "SIMPLE, FULL or PARTIAL", {
    "SIMPLE" => FkMatch::Simple,
    "FULL" => FkMatch::Full,
    "PARTIAL" => FkMatch::Partial,
});

keyword!(PolicyCommand, "ALL, SELECT, INSERT, UPDATE or DELETE", {
    "ALL" => PolicyCommand::All,
    "SELECT" => PolicyCommand::Select,
    "INSERT" => PolicyCommand::Insert,
    "UPDATE" => PolicyCommand::Update,
    "DELETE" => PolicyCommand::Delete,
});

keyword!(TriggerLevel, "ROW or STATEMENT", {
    "ROW" => TriggerLevel::Row,
    "STATEMENT" => TriggerLevel::Statement,
});

keyword!(TriggerEvent, "INSERT, UPDATE, DELETE or TRUNCATE", {
    "INSERT" => TriggerEvent::Insert,
    "UPDATE" => TriggerEvent::Update,
    "DELETE" => TriggerEvent::Delete,
    "TRUNCATE" => TriggerEvent::Truncate,
});

keyword!(TriggerEnabled, "true, false, ORIGIN, REPLICA, ALWAYS or DISABLED", {
    "ORIGIN" => TriggerEnabled::Origin,
    "REPLICA" => TriggerEnabled::Replica,
    "ALWAYS" => TriggerEnabled::Always,
    "DISABLED" => TriggerEnabled::Disabled,
});

keyword!(Volatility, "VOLATILE, STABLE or IMMUTABLE", {
    "VOLATILE" => Volatility::Volatile,
    "STABLE" => Volatility::Stable,
    "IMMUTABLE" => Volatility::Immutable,
});

keyword!(Parallel, "UNSAFE, RESTRICTED or SAFE", {
    "UNSAFE" => Parallel::Unsafe,
    "RESTRICTED" => Parallel::Restricted,
    "SAFE" => Parallel::Safe,
});

keyword!(ArgMode, "IN, OUT, INOUT or VARIADIC", {
    "IN" => ArgMode::In,
    "OUT" => ArgMode::Out,
    "INOUT" => ArgMode::InOut,
    "VARIADIC" => ArgMode::Variadic,
});

keyword!(IdentityGeneration, "ALWAYS or BY_DEFAULT", {
    "ALWAYS" => IdentityGeneration::Always,
    "BY_DEFAULT" => IdentityGeneration::ByDefault,
});

keyword!(ViewCheckOption, "LOCAL or CASCADED", {
    "LOCAL" => ViewCheckOption::Local,
    "CASCADED" => ViewCheckOption::Cascaded,
});

keyword!(NullsOrder, "FIRST or LAST", {
    "FIRST" => NullsOrder::First,
    "LAST" => NullsOrder::Last,
});

keyword!(Storage, "PLAIN, EXTERNAL, EXTENDED or MAIN", {
    "PLAIN" => Storage::Plain,
    "EXTERNAL" => Storage::External,
    "EXTENDED" => Storage::Extended,
    "MAIN" => Storage::Main,
});

/// `mode` of a `data` block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DataMode(pub DataSync);

keyword!(DataMode, "sync or insert", {
    "SYNC" => DataMode(DataSync::Sync),
    "INSERT" => DataMode(DataSync::Insert),
});

/// `as` of a policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PolicyKind {
    Permissive,
    Restrictive,
}

keyword!(PolicyKind, "PERMISSIVE or RESTRICTIVE", {
    "PERMISSIVE" => PolicyKind::Permissive,
    "RESTRICTIVE" => PolicyKind::Restrictive,
});

/// `security` of a routine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Security {
    Invoker,
    Definer,
}

keyword!(Security, "INVOKER or DEFINER", {
    "INVOKER" => Security::Invoker,
    "DEFINER" => Security::Definer,
});

/// `replica_identity` written as a keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReplicaKeyword {
    Default,
    Full,
    Nothing,
}

keyword!(ReplicaKeyword, "DEFAULT, FULL, NOTHING or an index", {
    "DEFAULT" => ReplicaKeyword::Default,
    "FULL" => ReplicaKeyword::Full,
    "NOTHING" => ReplicaKeyword::Nothing,
});

/// An entry of `privileges`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrivilegeKeyword {
    All,
    One(Privilege),
}

impl Keyword for PrivilegeKeyword {
    const EXPECTED: &'static str = "SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, \
                                    TRIGGER, MAINTAIN, USAGE, CREATE, EXECUTE or ALL";

    fn parse(word: &str) -> Option<Self> {
        let word = normalize(word);
        if word == "ALL" || word == "ALL_PRIVILEGES" {
            return Some(PrivilegeKeyword::All);
        }
        Privilege::parse(&word.replace('_', " ")).map(PrivilegeKeyword::One)
    }
}

/// Special grantees and policy roles that are not role names.
pub(super) const SPECIAL_ROLES: &[&str] =
    &["PUBLIC", "CURRENT_USER", "SESSION_USER", "CURRENT_ROLE"];

/// The special role a word names, matched case-insensitively.
pub(super) fn special_role(word: &str) -> Option<&'static str> {
    let upper = word.to_ascii_uppercase();
    SPECIAL_ROLES.iter().copied().find(|s| *s == upper)
}
