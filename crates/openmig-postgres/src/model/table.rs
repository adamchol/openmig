use super::{Acl, QualifiedName, SequenceOptions};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    /// In `attnum` order.
    pub columns: Vec<Column>,
    pub primary_key: Option<PrimaryKey>,
    pub foreign_keys: BTreeMap<String, ForeignKey>,
    pub uniques: BTreeMap<String, Unique>,
    pub checks: BTreeMap<String, Check>,
    pub exclusions: BTreeMap<String, Exclusion>,
    /// Every index, including those that implement constraints ([`Index::constraint`]).
    pub indexes: BTreeMap<String, Index>,
    pub policies: BTreeMap<String, Policy>,
    pub triggers: BTreeMap<String, Trigger>,
    /// `ENABLE ROW LEVEL SECURITY`.
    pub rls_enabled: bool,
    /// `FORCE ROW LEVEL SECURITY`.
    pub rls_forced: bool,
    pub replica_identity: ReplicaIdentity,
    pub unlogged: bool,
    /// `WITH (...)` storage parameters (`reloptions`).
    pub storage_params: BTreeMap<String, String>,
    /// `PARTITION BY` clause of a partitioned table, as `pg_get_partkeydef` prints it:
    /// `RANGE (created_at)`.
    pub partition_by: Option<String>,
    /// Set when this table is a partition.
    pub partition_of: Option<PartitionOf>,
    /// Parents of a table created with `INHERITS` (not partitions), in `inhseqno` order.
    pub inherits: Vec<QualifiedName>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

impl Table {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns.iter_mut().find(|c| c.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionOf {
    pub parent: QualifiedName,
    /// Partition bound as `pg_get_expr(relpartbound)` prints it:
    /// `FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')`, `DEFAULT`.
    pub bound: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReplicaIdentity {
    #[default]
    Default,
    Full,
    Nothing,
    /// `USING INDEX <name>`.
    Index(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// `format_type(atttypid, atttypmod)`: `integer`, `character varying(255)`, `text[]`,
    /// `public.citext`, `app.status`.
    pub data_type: String,
    pub not_null: bool,
    /// Default expression (`pg_get_expr`), never set for identity or generated columns.
    pub default: Option<String>,
    pub identity: Option<Identity>,
    pub generated: Option<Generated>,
    /// Collation when it differs from the type's default: `"C"`, `"en-x-icu"`.
    pub collation: Option<String>,
    /// Storage mode when it differs from the type's default.
    pub storage: Option<Storage>,
    /// Compression method when set explicitly: `pglz`, `lz4`.
    pub compression: Option<String>,
    pub comment: Option<String>,
    /// Column privileges.
    pub acl: Acl,
}

impl Column {
    pub fn new(name: impl Into<String>, data_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: data_type.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub generation: IdentityGeneration,
    /// Options of the identity sequence; its data type is the column's.
    pub sequence: SequenceOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityGeneration {
    Always,
    ByDefault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    pub expr: String,
    pub kind: GeneratedKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedKind {
    Stored,
    /// PostgreSQL 18+.
    Virtual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Plain,
    External,
    Extended,
    Main,
}

impl Storage {
    pub fn as_sql(self) -> &'static str {
        match self {
            Storage::Plain => "PLAIN",
            Storage::External => "EXTERNAL",
            Storage::Extended => "EXTENDED",
            Storage::Main => "MAIN",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrimaryKey {
    pub name: String,
    pub columns: Vec<String>,
    /// `INCLUDE (...)` columns.
    pub include: Vec<String>,
    pub deferrable: bool,
    pub initially_deferred: bool,
    /// Storage parameters of the backing index.
    pub storage_params: BTreeMap<String, String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: QualifiedName,
    pub ref_columns: Vec<String>,
    pub on_update: FkAction,
    pub on_delete: FkAction,
    pub match_type: FkMatch,
    pub deferrable: bool,
    pub initially_deferred: bool,
    /// False for a constraint added `NOT VALID` and not validated since.
    pub validated: bool,
    /// False for `NOT ENFORCED` (PostgreSQL 18+).
    pub enforced: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FkAction {
    #[default]
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl FkAction {
    pub fn as_sql(self) -> &'static str {
        match self {
            FkAction::NoAction => "NO ACTION",
            FkAction::Restrict => "RESTRICT",
            FkAction::Cascade => "CASCADE",
            FkAction::SetNull => "SET NULL",
            FkAction::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FkMatch {
    #[default]
    Simple,
    Full,
    Partial,
}

impl FkMatch {
    pub fn as_sql(self) -> &'static str {
        match self {
            FkMatch::Simple => "SIMPLE",
            FkMatch::Full => "FULL",
            FkMatch::Partial => "PARTIAL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unique {
    pub name: String,
    pub columns: Vec<String>,
    pub include: Vec<String>,
    /// False for `NULLS NOT DISTINCT` (PostgreSQL 15+).
    pub nulls_distinct: bool,
    pub deferrable: bool,
    pub initially_deferred: bool,
    /// Storage parameters of the backing index.
    pub storage_params: BTreeMap<String, String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// The condition without the `CHECK (...)` wrapper, as `pg_get_expr(conbin)` prints it.
    pub expr: String,
    pub no_inherit: bool,
    /// False for a constraint added `NOT VALID` and not validated since.
    pub validated: bool,
    /// False for `NOT ENFORCED` (PostgreSQL 18+).
    pub enforced: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exclusion {
    pub name: String,
    /// Index access method: `gist`, `spgist`, `btree`, `hash`.
    pub method: String,
    pub elements: Vec<ExclusionElement>,
    pub include: Vec<String>,
    /// `WHERE (...)` predicate.
    pub predicate: Option<String>,
    pub deferrable: bool,
    pub initially_deferred: bool,
    pub storage_params: BTreeMap<String, String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionElement {
    pub key: IndexKey,
    /// Operator as SQL text: `=`, `&&`, or `OPERATOR(ext.=)` outside `pg_catalog`.
    pub operator: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    /// Access method: `btree`, `hash`, `gin`, `gist`, `brin`, `spgist`, ...
    pub method: String,
    pub unique: bool,
    pub keys: Vec<IndexKey>,
    /// `INCLUDE (...)` columns.
    pub include: Vec<String>,
    /// Partial index predicate.
    pub predicate: Option<String>,
    /// False for `NULLS NOT DISTINCT` (PostgreSQL 15+).
    pub nulls_distinct: bool,
    pub storage_params: BTreeMap<String, String>,
    pub comment: Option<String>,
    /// The index implements the primary key, unique or exclusion constraint of the same name and
    /// is managed through that constraint.
    pub constraint: bool,
}

impl Default for Index {
    /// A non-unique btree index without keys.
    fn default() -> Self {
        Self {
            name: String::new(),
            method: "btree".into(),
            unique: false,
            keys: Vec::new(),
            include: Vec::new(),
            predicate: None,
            nulls_distinct: true,
            storage_params: BTreeMap::new(),
            comment: None,
            constraint: false,
        }
    }
}

/// One key part of an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexKey {
    pub expr: KeyExpr,
    /// Operator class when it is not the default for the key's type: `jsonb_path_ops`,
    /// `public.gin_trgm_ops`.
    pub opclass: Option<String>,
    /// Collation when it differs from the key's collation: `"C"`.
    pub collation: Option<String>,
    pub descending: bool,
    /// Null ordering when it is not the default for the direction (`NULLS LAST` ascending,
    /// `NULLS FIRST` descending).
    pub nulls: Option<NullsOrder>,
}

impl IndexKey {
    pub fn column(name: impl Into<String>) -> Self {
        Self {
            expr: KeyExpr::Column(name.into()),
            opclass: None,
            collation: None,
            descending: false,
            nulls: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyExpr {
    Column(String),
    /// An expression as `pg_get_indexdef(index, n, true)` prints it: `lower(email)`, `(a + 1)`.
    Expression(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NullsOrder {
    First,
    Last,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub name: String,
    pub command: PolicyCommand,
    /// False for `AS RESTRICTIVE`.
    pub permissive: bool,
    /// Sorted role names; [`super::PUBLIC`] for `PUBLIC`.
    pub roles: Vec<String>,
    pub using: Option<String>,
    pub check: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PolicyCommand {
    #[default]
    All,
    Select,
    Insert,
    Update,
    Delete,
}

impl PolicyCommand {
    pub fn as_sql(self) -> &'static str {
        match self {
            PolicyCommand::All => "ALL",
            PolicyCommand::Select => "SELECT",
            PolicyCommand::Insert => "INSERT",
            PolicyCommand::Update => "UPDATE",
            PolicyCommand::Delete => "DELETE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: BTreeSet<TriggerEvent>,
    /// `UPDATE OF` columns, in `tgattr` order.
    pub update_of: Vec<String>,
    pub level: TriggerLevel,
    /// The `WHEN` condition without the surrounding parentheses.
    pub when: Option<String>,
    /// The trigger function (which takes no arguments).
    pub function: QualifiedName,
    pub args: Vec<String>,
    pub enabled: TriggerEnabled,
    /// Set for `CREATE CONSTRAINT TRIGGER`.
    pub constraint: Option<ConstraintTrigger>,
    /// `REFERENCING OLD TABLE AS ...`.
    pub old_table: Option<String>,
    /// `REFERENCING NEW TABLE AS ...`.
    pub new_table: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstraintTrigger {
    pub deferrable: bool,
    pub initially_deferred: bool,
    /// `FROM` referenced table.
    pub from: Option<QualifiedName>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

impl TriggerTiming {
    pub fn as_sql(self) -> &'static str {
        match self {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
            TriggerTiming::InsteadOf => "INSTEAD OF",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

impl TriggerEvent {
    pub fn as_sql(self) -> &'static str {
        match self {
            TriggerEvent::Insert => "INSERT",
            TriggerEvent::Update => "UPDATE",
            TriggerEvent::Delete => "DELETE",
            TriggerEvent::Truncate => "TRUNCATE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerLevel {
    Row,
    Statement,
}

/// `session_replication_role` firing mode of a trigger or event trigger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriggerEnabled {
    /// `ENABLE`: fires in `origin` and `local` modes.
    #[default]
    Origin,
    /// `ENABLE REPLICA`.
    Replica,
    /// `ENABLE ALWAYS`.
    Always,
    /// `DISABLE`.
    Disabled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub name: String,
    /// `pg_get_viewdef(oid, true)`.
    pub query: String,
    /// Derived from the query.
    pub columns: Vec<ViewColumn>,
    /// PostgreSQL 15+.
    pub security_invoker: bool,
    pub security_barrier: bool,
    pub check_option: Option<ViewCheckOption>,
    /// `INSTEAD OF` (and statement-level) triggers.
    pub triggers: BTreeMap<String, Trigger>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewColumn {
    pub name: String,
    pub data_type: String,
    pub comment: Option<String>,
    /// Column privileges.
    pub acl: Acl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewCheckOption {
    Local,
    Cascaded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedView {
    pub name: String,
    /// `pg_get_viewdef(oid, true)`.
    pub query: String,
    pub columns: Vec<ViewColumn>,
    /// False for `WITH NO DATA` (not populated).
    pub with_data: bool,
    pub indexes: BTreeMap<String, Index>,
    pub storage_params: BTreeMap<String, String>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

impl Default for MaterializedView {
    /// A populated materialized view.
    fn default() -> Self {
        Self {
            name: String::new(),
            query: String::new(),
            columns: Vec::new(),
            with_data: true,
            indexes: BTreeMap::new(),
            storage_params: BTreeMap::new(),
            comment: None,
            owner: String::new(),
            acl: Acl::new(),
        }
    }
}
