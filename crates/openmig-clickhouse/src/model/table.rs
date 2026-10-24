use super::QualifiedName;
use std::collections::BTreeMap;
use std::fmt;

/// A table (any engine except views and dictionaries).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    /// The engine expression without its clauses:
    /// `ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)`,
    /// `MergeTree`, `TimeSeries`.
    pub engine: String,
    /// In declaration order. Empty for `TimeSeries` tables, whose columns are implicit.
    pub columns: Vec<Column>,
    /// `PARTITION BY` expression: `toYYYYMM(created)`, `(toYYYYMM(d), tenant)`.
    pub partition_by: Option<String>,
    /// Elements of the sorting key: `ORDER BY (a, b DESC)` is `["a", "b DESC"]`; empty for
    /// `ORDER BY tuple()` and for engines without a sorting key.
    pub order_by: Vec<String>,
    /// Elements of the primary key. Equal to [`Table::order_by`] when the table declares no
    /// separate `PRIMARY KEY`, as the server treats both the same.
    pub primary_key: Vec<String>,
    /// `SAMPLE BY` expression.
    pub sample_by: Option<String>,
    /// Table TTL: every TTL element as one string,
    /// `created + toIntervalDay(30), created + toIntervalDay(7) TO DISK 'cold'`.
    pub ttl: Option<String>,
    /// `SETTINGS` as the server prints them, values as SQL literals (`'encrypted'`, `8192`).
    /// Includes settings the server adds itself, such as `index_granularity = 8192`.
    pub settings: BTreeMap<String, String>,
    /// Data-skipping indexes.
    pub indexes: BTreeMap<String, Index>,
    pub projections: BTreeMap<String, Projection>,
    pub constraints: BTreeMap<String, Constraint>,
    /// Target tables of a `TimeSeries` table.
    pub time_series: Option<TimeSeriesTargets>,
    pub comment: Option<String>,
}

impl Table {
    pub fn new(name: impl Into<String>, engine: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            engine: engine.into(),
            ..Self::default()
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The engine name without arguments: `ReplicatedMergeTree`.
    pub fn engine_name(&self) -> &str {
        super::engine_name(&self.engine)
    }

    /// Whether the engine belongs to the MergeTree family (and so needs an `ORDER BY`).
    pub fn is_merge_tree(&self) -> bool {
        self.engine_name().ends_with("MergeTree")
    }

    /// Whether the table uses a `Replicated*MergeTree` engine.
    pub fn is_replicated(&self) -> bool {
        let name = self.engine_name();
        name.starts_with("Replicated") && name.ends_with("MergeTree")
    }
}

/// One column of a table or materialized view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The type as the server prints it; nullability is part of it: `Nullable(String)`,
    /// `LowCardinality(String)`, `DateTime64(6, 'UTC')`.
    pub data_type: String,
    pub default: Option<ColumnDefault>,
    /// Codec elements without `CODEC(...)`: `["Delta(8)", "ZSTD(1)"]`. Empty for the server
    /// default.
    pub codec: Vec<String>,
    /// Column TTL expression.
    pub ttl: Option<String>,
    /// Explicit statistics types without `STATISTICS(...)`: `["tdigest"]`.
    pub statistics: Vec<String>,
    /// Column-level `SETTINGS (...)`, values as SQL literals.
    pub settings: BTreeMap<String, String>,
    pub comment: Option<String>,
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

/// `DEFAULT`, `MATERIALIZED`, `ALIAS` or `EPHEMERAL` with its expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefault {
    pub kind: DefaultKind,
    /// The expression as the server prints it. A bare `EPHEMERAL` is reported with the type's
    /// default value, `defaultValueOfTypeName('String')`.
    pub expression: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefaultKind {
    Default,
    Materialized,
    Alias,
    Ephemeral,
}

impl DefaultKind {
    pub fn as_sql(self) -> &'static str {
        match self {
            DefaultKind::Default => "DEFAULT",
            DefaultKind::Materialized => "MATERIALIZED",
            DefaultKind::Alias => "ALIAS",
            DefaultKind::Ephemeral => "EPHEMERAL",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "DEFAULT" => Some(DefaultKind::Default),
            "MATERIALIZED" => Some(DefaultKind::Materialized),
            "ALIAS" => Some(DefaultKind::Alias),
            "EPHEMERAL" => Some(DefaultKind::Ephemeral),
            _ => None,
        }
    }
}

impl fmt::Display for DefaultKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_sql())
    }
}

/// A data-skipping index: `INDEX name expression TYPE index_type GRANULARITY granularity`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub expression: String,
    /// Type with arguments: `minmax`, `set(1024)`, `bloom_filter(0.01)`,
    /// `tokenbf_v1(32768, 3, 0)`.
    pub index_type: String,
    pub granularity: u64,
}

/// `PROJECTION name (query) [WITH SETTINGS (...)]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    pub query: String,
    pub settings: BTreeMap<String, String>,
}

/// `CONSTRAINT name CHECK expression` or `CONSTRAINT name ASSUME expression`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub expression: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstraintKind {
    Check,
    Assume,
}

impl ConstraintKind {
    pub fn as_sql(self) -> &'static str {
        match self {
            ConstraintKind::Check => "CHECK",
            ConstraintKind::Assume => "ASSUME",
        }
    }
}

/// The target clauses of a `TimeSeries` table, as printed after `DATA`, `TAGS` and `METRICS`:
/// `ENGINE = MergeTree ORDER BY (id, timestamp)` for an inner table, or a table name for an
/// external target. The inner tables themselves are never part of the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeSeriesTargets {
    pub data: Option<String>,
    pub tags: Option<String>,
    pub metrics: Option<String>,
}

/// A view: `CREATE VIEW name [SQL SECURITY ...] AS query`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub name: String,
    /// The query as the server prints it (`system.tables.as_select`).
    pub query: String,
    /// `DEFINER`, `INVOKER` or `NONE`; `None` when the view does not declare it.
    pub sql_security: Option<String>,
    /// `DEFINER = user`; `None` when absent or equal to the connecting user, so that objects
    /// created by whoever applies the schema compare equal.
    pub definer: Option<String>,
    pub comment: Option<String>,
}

/// A materialized view, either writing `TO` an existing table or owning an inner table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterializedView {
    pub name: String,
    /// The `TO` target table.
    pub to: Option<QualifiedName>,
    /// The query as the server prints it (`system.tables.as_select`).
    pub query: String,
    /// Columns as the server recorded them when the view was created. They do not follow later
    /// changes of the source tables, so they show when a view must be recreated.
    pub columns: Vec<Column>,
    /// Refresh clause of a refreshable view, without `REFRESH`:
    /// `EVERY 1 HOUR`, `AFTER 30 SECOND APPEND`.
    pub refresh: Option<String>,
    /// Engine and clauses of the inner table of a view without `TO`.
    pub inner: Option<InnerTable>,
    /// See [`View::sql_security`].
    pub sql_security: Option<String>,
    /// See [`View::definer`].
    pub definer: Option<String>,
    pub comment: Option<String>,
}

/// Storage of a materialized view's inner table; fields as in [`Table`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InnerTable {
    pub engine: String,
    pub partition_by: Option<String>,
    pub order_by: Vec<String>,
    pub primary_key: Vec<String>,
    pub sample_by: Option<String>,
    pub ttl: Option<String>,
    pub settings: BTreeMap<String, String>,
}

/// A dictionary: `CREATE DICTIONARY`. Clause contents are SQL text as the server prints them;
/// secrets in sources are masked by the server (`[HIDDEN]`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dictionary {
    pub name: String,
    pub attributes: Vec<DictionaryAttribute>,
    pub primary_key: Vec<String>,
    /// `CLICKHOUSE(TABLE 'events' DB 'app')`.
    pub source: String,
    /// `MIN 0 MAX 300`.
    pub lifetime: Option<String>,
    /// `HASHED()`.
    pub layout: String,
    /// `MIN start MAX end` for range-hashed layouts.
    pub range: Option<String>,
    pub settings: BTreeMap<String, String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictionaryAttribute {
    pub name: String,
    pub data_type: String,
    pub default: Option<String>,
    pub expression: Option<String>,
    pub hierarchical: bool,
    pub bidirectional: bool,
    pub injective: bool,
    pub is_object_id: bool,
}
