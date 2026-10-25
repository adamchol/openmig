//! The ClickHouse schema model.
//!
//! The same types are produced by decoding HCL and by inspecting a live server. Inspected values
//! are canonical: names are raw (unquoted) identifiers, while engines, types, expressions, codecs,
//! TTLs and setting values are SQL text exactly as the server prints it in
//! `system.tables.create_table_query` (macros such as `{uuid}` and `{replica}` stay unexpanded,
//! `INTERVAL 1 DAY` becomes `toIntervalDay(1)`, default engine arguments are filled in). The
//! same schema created on two servers of the same version inspects to equal models; UUIDs,
//! metadata paths, sizes and timestamps are never part of the model.
//!
//! Unordered collections are `BTreeMap`s keyed by name; ordered ones (columns, key elements,
//! codecs) are `Vec`s; grants are sorted `Vec`s (see [`Grant`]).

mod access;
mod objects;
mod table;

pub use access::{
    Grant, GrantLevel, Grantee, Role, RoleGrant, RoleSet, RowPolicy, RowPolicyId, User,
};
pub use objects::Object;
pub use table::{
    Column, ColumnDefault, Constraint, ConstraintKind, DefaultKind, Dictionary,
    DictionaryAttribute, Index, InnerTable, MaterializedView, Projection, Table, View,
};

use openmig_core::ObjectRef;
use openmig_core::sql::{Dialect, quote_ident};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A database-qualified name with raw (unquoted) parts.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedName {
    pub database: String,
    pub name: String,
}

impl QualifiedName {
    pub fn new(database: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            database: database.into(),
            name: name.into(),
        }
    }

    /// `` `database`.`name` ``.
    pub fn to_sql(&self) -> String {
        format!(
            "{}.{}",
            quote_ident(&self.database, Dialect::ClickHouse),
            quote_ident(&self.name, Dialect::ClickHouse)
        )
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.database, self.name)
    }
}

/// Everything OpenMig manages on one ClickHouse server (or cluster): databases and their
/// contents, plus the server-wide access entities and SQL functions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub databases: BTreeMap<String, Database>,
    /// SQL user-defined functions (`CREATE FUNCTION`).
    pub functions: BTreeMap<String, Function>,
    /// Roles stored in SQL-managed user directories. Empty unless roles are managed
    /// (`Scope::manage_roles`).
    pub roles: BTreeMap<String, Role>,
    /// Users stored in SQL-managed user directories, without the connecting user and `default`.
    /// Read by inspection but removed by [`crate::restrict::restrict`]: users are not managed.
    pub users: BTreeMap<String, User>,
    /// Privilege grants of managed grantees, in canonical form: one entry per grantee, object,
    /// grant option and set of columns, privileges collapsed with the server's privilege
    /// hierarchy. Sorted (see [`Grant`]).
    pub grants: Vec<Grant>,
    /// Role memberships (`GRANT role TO grantee`) of managed grantees. Sorted.
    pub role_grants: Vec<RoleGrant>,
    pub row_policies: BTreeMap<RowPolicyId, RowPolicy>,
    /// "`from` depends on `to`" edges between objects of this model. Sorted and free of
    /// duplicates (a hard edge wins over a soft one).
    pub dependencies: Vec<Dependency>,
    /// `major.minor` of the server the model was inspected from (`0.0` for decoded models). The
    /// planner uses the current side's version for version-dependent syntax.
    pub server_version: (u32, u32),
    /// Roles that grants and row policies of a decoded schema name but that the schema does not
    /// declare. They are created in scratch servers only and never managed. Always empty for
    /// inspected models.
    pub external_roles: BTreeSet<String>,
}

/// One database and everything in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Database {
    pub name: String,
    /// Engine expression from `system.databases.engine_full`: `Atomic`,
    /// `Replicated('/clickhouse/databases/app', '{shard}', '{replica}')`.
    pub engine: String,
    pub comment: Option<String>,
    pub tables: BTreeMap<String, Table>,
    pub views: BTreeMap<String, View>,
    pub materialized_views: BTreeMap<String, MaterializedView>,
    pub dictionaries: BTreeMap<String, Dictionary>,
}

impl Database {
    pub fn new(name: impl Into<String>, engine: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            engine: engine.into(),
            ..Self::default()
        }
    }

    /// True when the database contains no objects.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
            && self.views.is_empty()
            && self.materialized_views.is_empty()
            && self.dictionaries.is_empty()
    }

    /// The engine name without arguments: `Replicated`, `Atomic`.
    pub fn engine_name(&self) -> &str {
        engine_name(&self.engine)
    }

    /// Whether DDL for tables in this database is replicated by the database engine (and so must
    /// not use `ON CLUSTER`).
    pub fn is_replicated(&self) -> bool {
        self.engine_name() == "Replicated"
    }
}

/// A SQL user-defined function: `CREATE FUNCTION name AS (parameters) -> body`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub parameters: Vec<String>,
    /// The lambda body as the server prints it: `((k * x) + b)`.
    pub body: String,
}

/// `from` depends on `to`: `to` must exist before `from` is created, and `from` must be dropped
/// (and recreated) when `to` is dropped or changed incompatibly.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dependency {
    pub from: ObjectRef,
    pub to: ObjectRef,
    /// Guessed from identifiers in SQL text (view queries, default expressions) rather than
    /// reported by the server or implied by the definition. The planner drops soft edges that
    /// would form a cycle.
    pub soft: bool,
}

impl Model {
    /// True when the model contains no managed objects. An empty `default` database is ignored
    /// because every server has one.
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
            && self.roles.is_empty()
            && self.users.is_empty()
            && self.grants.is_empty()
            && self.role_grants.is_empty()
            && self.row_policies.is_empty()
            && self
                .databases
                .values()
                .all(|d| d.name == "default" && d.is_empty())
    }

    pub fn database(&self, name: &str) -> Option<&Database> {
        self.databases.get(name)
    }

    pub fn table(&self, name: &QualifiedName) -> Option<&Table> {
        self.databases.get(&name.database)?.tables.get(&name.name)
    }

    pub fn view(&self, name: &QualifiedName) -> Option<&View> {
        self.databases.get(&name.database)?.views.get(&name.name)
    }

    pub fn materialized_view(&self, name: &QualifiedName) -> Option<&MaterializedView> {
        self.databases
            .get(&name.database)?
            .materialized_views
            .get(&name.name)
    }

    pub fn dictionary(&self, name: &QualifiedName) -> Option<&Dictionary> {
        self.databases
            .get(&name.database)?
            .dictionaries
            .get(&name.name)
    }

    /// Sort [`Model::grants`], [`Model::role_grants`] and [`Model::dependencies`] and remove
    /// duplicates, keeping the hard edge when an edge is both hard and soft.
    pub fn normalize(&mut self) {
        self.grants.sort();
        self.grants.dedup();
        self.role_grants.sort();
        self.role_grants.dedup();
        self.dependencies.sort();
        self.dependencies.dedup_by(|b, a| {
            if a.from == b.from && a.to == b.to {
                a.soft &= b.soft;
                true
            } else {
                false
            }
        });
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

/// The identifier at the start of an engine expression: `ReplicatedMergeTree` for
/// `ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')`.
pub fn engine_name(engine: &str) -> &str {
    let end = engine
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(engine.len());
    &engine[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmig_core::ObjectKind;

    #[test]
    fn engine_names() {
        assert_eq!(
            engine_name("ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')"),
            "ReplicatedMergeTree"
        );
        assert_eq!(engine_name("Atomic"), "Atomic");
        assert!(Database::new("d", "Replicated('/p', '{shard}', '{replica}')").is_replicated());
        assert!(!Database::new("d", "Atomic").is_replicated());
    }

    #[test]
    fn emptiness() {
        let mut model = Model::default();
        assert!(model.is_empty());
        model
            .databases
            .insert("default".into(), Database::new("default", "Atomic"));
        assert!(model.is_empty());
        model
            .databases
            .insert("app".into(), Database::new("app", "Atomic"));
        assert!(!model.is_empty());
    }

    #[test]
    fn normalize_prefers_hard_edges() {
        let a = ObjectRef::new(ObjectKind::View, ["d", "v"]);
        let b = ObjectRef::new(ObjectKind::Table, ["d", "t"]);
        let mut model = Model {
            dependencies: vec![
                Dependency {
                    from: a.clone(),
                    to: b.clone(),
                    soft: true,
                },
                Dependency {
                    from: a.clone(),
                    to: b.clone(),
                    soft: false,
                },
                Dependency {
                    from: a.clone(),
                    to: b.clone(),
                    soft: true,
                },
            ],
            ..Model::default()
        };
        model.normalize();
        assert_eq!(
            model.dependencies,
            vec![Dependency {
                from: a.clone(),
                to: b.clone(),
                soft: false
            }]
        );
        assert_eq!(model.dependencies_of(&a).collect::<Vec<_>>(), vec![&b]);
        assert_eq!(model.dependants_of(&b).collect::<Vec<_>>(), vec![&a]);
    }
}
