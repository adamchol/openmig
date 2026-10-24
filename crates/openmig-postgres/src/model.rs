//! The PostgreSQL schema model.
//!
//! The same types are produced by decoding HCL and by inspecting a live database. Inspected values
//! are canonical: names are raw (unquoted) identifiers, while types, expressions, collations and
//! operator classes are SQL text exactly as PostgreSQL prints it with an empty `search_path`, so
//! everything outside `pg_catalog` is schema-qualified (`public.citext`, `app.status`). The same
//! schema created in two databases on the same server major inspects to equal models.
//!
//! Unordered collections are `BTreeMap`s keyed by name (functions by signature, see
//! [`Function::signature`]); ordered ones (columns, enum values, arguments, index keys) are `Vec`s.

mod acl;
mod cluster;
mod objects;
mod routine;
mod table;
mod types;

pub use acl::{Acl, AclItem, PUBLIC, Privilege, default_acl};
pub use cluster::{EventTrigger, Extension, Membership, Role};
pub use objects::Object;
pub use routine::{
    ArgMode, Function, FunctionArg, FunctionBody, Parallel, ReturnColumn, Returns, Volatility,
    function_signature,
};
pub use table::{
    Check, Column, ConstraintTrigger, Exclusion, ExclusionElement, FkAction, FkMatch, ForeignKey,
    Generated, GeneratedKind, Identity, IdentityGeneration, Index, IndexKey, KeyExpr,
    MaterializedView, NullsOrder, PartitionOf, Policy, PolicyCommand, PrimaryKey, ReplicaIdentity,
    Storage, Table, Trigger, TriggerEnabled, TriggerEvent, TriggerLevel, TriggerTiming, Unique,
    View, ViewCheckOption, ViewColumn,
};
pub use types::{Composite, CompositeField, Domain, DomainCheck, Enum, Sequence, SequenceOptions};

use openmig_core::ObjectRef;
use openmig_core::sql::{Dialect, quote_ident};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A schema-qualified name with raw (unquoted) parts.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedName {
    pub schema: String,
    pub name: String,
}

impl QualifiedName {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// `"schema"."name"`.
    pub fn to_sql(&self) -> String {
        format!(
            "{}.{}",
            quote_ident(&self.schema, Dialect::Postgres),
            quote_ident(&self.name, Dialect::Postgres)
        )
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

/// A column of a table, identified by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnRef {
    pub schema: String,
    pub table: String,
    pub column: String,
}

/// One database: everything OpenMig manages in it plus the cluster-wide roles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Database {
    pub schemas: BTreeMap<String, Schema>,
    /// Installed extensions except `plpgsql`. Objects that belong to an extension are not part
    /// of the model.
    pub extensions: BTreeMap<String, Extension>,
    /// Cluster-wide roles. Empty unless roles are managed (`Scope::manage_roles`).
    pub roles: BTreeMap<String, Role>,
    pub event_triggers: BTreeMap<String, EventTrigger>,
    /// Rows of data-managed tables (`Scope::data_tables`).
    pub data: BTreeMap<QualifiedName, TableData>,
    /// "`from` depends on `to`" edges between objects of this model, from `pg_depend` (and
    /// policy roles from `pg_shdepend`). Sorted and free of duplicates. Dependencies on objects
    /// that belong to an extension point to the extension. Left out: edges from an object to its
    /// own container (an index to its table, a function to its schema), edges to system objects,
    /// and `OWNED BY` links (see [`Sequence::owned_by`]). An index that implements a constraint
    /// is represented by the constraint.
    pub dependencies: Vec<Dependency>,
    /// Major version of the server the model was inspected from (`0` for decoded models). The
    /// planner uses the current side's version for version-dependent syntax.
    pub server_major: u32,
    /// Roles that grants and policies of a decoded schema name but that the schema does not
    /// declare. They are created (`NOLOGIN`) in scratch databases only and never managed.
    /// Always empty for inspected models.
    pub external_roles: BTreeSet<String>,
}

/// A namespace and everything in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub comment: Option<String>,
    pub acl: Acl,
    pub tables: BTreeMap<String, Table>,
    pub views: BTreeMap<String, View>,
    pub materialized_views: BTreeMap<String, MaterializedView>,
    /// Keyed by [`Function::signature`].
    pub functions: BTreeMap<String, Function>,
    /// Keyed by [`Function::signature`].
    pub procedures: BTreeMap<String, Function>,
    /// Standalone sequences; identity sequences belong to their column.
    pub sequences: BTreeMap<String, Sequence>,
    pub enums: BTreeMap<String, Enum>,
    pub composites: BTreeMap<String, Composite>,
    pub domains: BTreeMap<String, Domain>,
}

impl Schema {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// True when the schema contains no objects.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
            && self.views.is_empty()
            && self.materialized_views.is_empty()
            && self.functions.is_empty()
            && self.procedures.is_empty()
            && self.sequences.is_empty()
            && self.enums.is_empty()
            && self.composites.is_empty()
            && self.domains.is_empty()
    }
}

/// Rows of a data-managed table, in PostgreSQL's text form (`col::text`). Inspection reads the
/// key columns and the columns listed in [`openmig_core::DataTable::columns`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableData {
    /// Columns whose values identify a row: the primary key, or every column when the table has
    /// none.
    pub key: Vec<String>,
    /// Rows by the values of the key columns. Each row maps the managed columns to their value
    /// (`None` for NULL).
    pub rows: BTreeMap<Vec<Option<String>>, BTreeMap<String, Option<String>>>,
    /// The `mode` of the declaring `data` block (decoded models only; `None` = configured
    /// default). Inspection leaves it `None`; the diff takes the mode from the scope.
    pub mode: Option<openmig_core::DataSync>,
}

/// `from` depends on `to`: `to` must exist before `from` is created, and `from` must be dropped
/// (and recreated) when `to` is dropped or changed incompatibly.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dependency {
    pub from: ObjectRef,
    pub to: ObjectRef,
}

impl Database {
    /// True when the database contains no managed objects. The `public` schema, roles and
    /// table data are ignored: every new database has the former, the latter are cluster-wide or
    /// cannot exist without tables.
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
            && self.event_triggers.is_empty()
            && self
                .schemas
                .values()
                .all(|s| s.name == "public" && s.is_empty())
    }

    pub fn schema(&self, name: &str) -> Option<&Schema> {
        self.schemas.get(name)
    }

    pub fn table(&self, name: &QualifiedName) -> Option<&Table> {
        self.schemas.get(&name.schema)?.tables.get(&name.name)
    }

    /// Sort and de-duplicate [`Database::dependencies`].
    pub fn normalize_dependencies(&mut self) {
        self.dependencies.sort();
        self.dependencies.dedup();
    }

    /// Objects that `object` depends on.
    pub fn dependencies_of<'a>(
        &'a self,
        object: &'a ObjectRef,
    ) -> impl Iterator<Item = &'a ObjectRef> + 'a {
        self.dependencies
            .iter()
            .filter(move |d| &d.from == object)
            .map(|d| &d.to)
    }

    /// Objects that depend on `object` (directly).
    pub fn dependants_of<'a>(
        &'a self,
        object: &'a ObjectRef,
    ) -> impl Iterator<Item = &'a ObjectRef> + 'a {
        self.dependencies
            .iter()
            .filter(move |d| &d.to == object)
            .map(|d| &d.from)
    }
}
