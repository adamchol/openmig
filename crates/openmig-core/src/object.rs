use std::fmt;

/// Every kind of object either engine manages. Used for excludes (`table:app.*`), rename hints,
/// statement origins and messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Database,
    Schema,
    Extension,
    Role,
    User,
    Table,
    Column,
    Index,
    PrimaryKey,
    ForeignKey,
    Unique,
    Check,
    Exclusion,
    Constraint,
    View,
    MaterializedView,
    Function,
    Procedure,
    Aggregate,
    Trigger,
    EventTrigger,
    Policy,
    RowPolicy,
    Enum,
    Composite,
    Domain,
    Sequence,
    Grant,
    Data,
    Dictionary,
    Projection,
}

impl ObjectKind {
    pub const ALL: &'static [ObjectKind] = &[
        ObjectKind::Database,
        ObjectKind::Schema,
        ObjectKind::Extension,
        ObjectKind::Role,
        ObjectKind::User,
        ObjectKind::Table,
        ObjectKind::Column,
        ObjectKind::Index,
        ObjectKind::PrimaryKey,
        ObjectKind::ForeignKey,
        ObjectKind::Unique,
        ObjectKind::Check,
        ObjectKind::Exclusion,
        ObjectKind::Constraint,
        ObjectKind::View,
        ObjectKind::MaterializedView,
        ObjectKind::Function,
        ObjectKind::Procedure,
        ObjectKind::Aggregate,
        ObjectKind::Trigger,
        ObjectKind::EventTrigger,
        ObjectKind::Policy,
        ObjectKind::RowPolicy,
        ObjectKind::Enum,
        ObjectKind::Composite,
        ObjectKind::Domain,
        ObjectKind::Sequence,
        ObjectKind::Grant,
        ObjectKind::Data,
        ObjectKind::Dictionary,
        ObjectKind::Projection,
    ];

    /// The name used in HCL blocks, excludes and messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Database => "database",
            ObjectKind::Schema => "schema",
            ObjectKind::Extension => "extension",
            ObjectKind::Role => "role",
            ObjectKind::User => "user",
            ObjectKind::Table => "table",
            ObjectKind::Column => "column",
            ObjectKind::Index => "index",
            ObjectKind::PrimaryKey => "primary_key",
            ObjectKind::ForeignKey => "foreign_key",
            ObjectKind::Unique => "unique",
            ObjectKind::Check => "check",
            ObjectKind::Exclusion => "exclude",
            ObjectKind::Constraint => "constraint",
            ObjectKind::View => "view",
            ObjectKind::MaterializedView => "materialized_view",
            ObjectKind::Function => "function",
            ObjectKind::Procedure => "procedure",
            ObjectKind::Aggregate => "aggregate",
            ObjectKind::Trigger => "trigger",
            ObjectKind::EventTrigger => "event_trigger",
            ObjectKind::Policy => "policy",
            ObjectKind::RowPolicy => "row_policy",
            ObjectKind::Enum => "enum",
            ObjectKind::Composite => "composite",
            ObjectKind::Domain => "domain",
            ObjectKind::Sequence => "sequence",
            ObjectKind::Grant => "grant",
            ObjectKind::Data => "data",
            ObjectKind::Dictionary => "dictionary",
            ObjectKind::Projection => "projection",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity of one database object, independent of engine.
///
/// `path` is the object's name qualified by its containers, e.g. `["app", "users"]` for a table,
/// `["app", "users", "email"]` for a column, `["web"]` for a role, and
/// `["app", "f(integer, text)"]` for a function overload.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    pub kind: ObjectKind,
    pub path: Vec<String>,
}

impl ObjectRef {
    pub fn new<S: Into<String>>(kind: ObjectKind, path: impl IntoIterator<Item = S>) -> Self {
        Self {
            kind,
            path: path.into_iter().map(Into::into).collect(),
        }
    }

    /// `app.users`.
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }

    pub fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind, self.dotted())
    }
}
