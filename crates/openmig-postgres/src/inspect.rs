//! Reading a live database into the model.
//!
//! A fixed set of catalog queries (one per object kind) is pipelined on the session's connection,
//! then the rows are assembled into a [`Database`]. The session must use the inspection settings
//! (empty `search_path`, UTC, ...), see `PgSession::inspect`. Excludes are applied at the end with
//! [`crate::restrict::restrict`], so inspection and restriction of a decoded model agree.

mod cluster;
mod data;
mod deps;
mod relations;
mod routines;
mod sql;
mod text;
mod types;

use crate::conn::describe;
use crate::model::{Acl, AclItem, Database, Privilege, Schema};
use openmig_core::{Error, ObjectKind, ObjectRef, Scope};
use std::collections::BTreeMap;
use tokio_postgres::types::{Json, ToSql, Type};
use tokio_postgres::{Client, Row};

pub(crate) struct InspectContext {
    /// Server major version.
    pub major: u32,
    /// Schema of the history table; never inspected.
    pub history_schema: String,
}

/// Catalogs that `pg_depend` rows refer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SystemCatalog {
    Class,
    Type,
    Proc,
    Constraint,
    Trigger,
    Policy,
    Rewrite,
    AttrDef,
    Namespace,
    Extension,
    EventTrigger,
    Other,
}

impl SystemCatalog {
    fn parse(regclass: &str) -> Self {
        match regclass {
            "pg_class" => SystemCatalog::Class,
            "pg_type" => SystemCatalog::Type,
            "pg_proc" => SystemCatalog::Proc,
            "pg_constraint" => SystemCatalog::Constraint,
            "pg_trigger" => SystemCatalog::Trigger,
            "pg_policy" => SystemCatalog::Policy,
            "pg_rewrite" => SystemCatalog::Rewrite,
            "pg_attrdef" => SystemCatalog::AttrDef,
            "pg_namespace" => SystemCatalog::Namespace,
            "pg_extension" => SystemCatalog::Extension,
            "pg_event_trigger" => SystemCatalog::EventTrigger,
            _ => SystemCatalog::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelKind {
    Table,
    View,
    MaterializedView,
    Sequence,
    /// A sequence that implements an identity column; part of the column in the model.
    IdentitySequence,
    /// The relation behind a composite type.
    Composite,
}

#[derive(Clone, Debug)]
struct Relation {
    schema: String,
    name: String,
    kind: RelKind,
}

impl Relation {
    fn object_ref(&self) -> Option<ObjectRef> {
        let kind = match self.kind {
            RelKind::Table => ObjectKind::Table,
            RelKind::View => ObjectKind::View,
            RelKind::MaterializedView => ObjectKind::MaterializedView,
            RelKind::Sequence => ObjectKind::Sequence,
            RelKind::Composite => ObjectKind::Composite,
            RelKind::IdentitySequence => return None,
        };
        Some(ObjectRef::new(kind, [&self.schema, &self.name]))
    }
}

/// What catalog objects (by oid) are in the model, for assembling rows and resolving
/// dependencies.
#[derive(Default)]
struct Catalog {
    namespaces: BTreeMap<u32, String>,
    relations: BTreeMap<u32, Relation>,
    /// `(relation, attnum)` to attribute name.
    columns: BTreeMap<(u32, i16), String>,
    /// Types, functions, constraints, triggers, policies, indexes (as their constraint when they
    /// implement one), extensions, event triggers.
    objects: BTreeMap<(SystemCatalog, u32), ObjectRef>,
    /// Array types (element type) and row types (relation): `(elem, rel)`.
    derived_types: BTreeMap<u32, (u32, u32)>,
    /// Rewrite rules and column defaults: `(relation, attnum)`.
    relation_parts: BTreeMap<(SystemCatalog, u32), (u32, i16)>,
    /// Objects that belong to an extension, by extension name.
    extension_members: BTreeMap<(SystemCatalog, u32), String>,
}

impl Catalog {
    fn schema(&self, oid: u32) -> Option<&str> {
        self.namespaces.get(&oid).map(String::as_str)
    }
}

fn err(what: &'static str) -> impl Fn(tokio_postgres::Error) -> Error {
    move |e| Error::Inspect(format!("{what}: {}", describe(&e)))
}

async fn query(
    client: &Client,
    what: &'static str,
    sql: &str,
    schemas: Option<&[u32]>,
) -> Result<Vec<Row>, Error> {
    let params: Vec<(&(dyn ToSql + Sync), Type)> = match &schemas {
        Some(oids) => vec![(oids as &(dyn ToSql + Sync), Type::OID_ARRAY)],
        None => Vec::new(),
    };
    client.query_typed(sql, &params).await.map_err(err(what))
}

/// Read the database the client is connected to.
pub(crate) async fn inspect(
    client: &Client,
    ctx: &InspectContext,
    scope: &Scope,
) -> Result<Database, Error> {
    let q = sql::Queries::new(ctx.major);
    let schema_rows = client
        .query_typed(&q.schemas, &[(&ctx.history_schema, Type::TEXT)])
        .await
        .map_err(err("schemas"))?;
    let mut db = Database {
        server_major: ctx.major,
        ..Database::default()
    };
    let mut cat = Catalog::default();
    for row in &schema_rows {
        let name: String = row.get("name");
        if !scope.schemas.includes(&name) {
            continue;
        }
        cat.namespaces.insert(row.get("oid"), name.clone());
        db.schemas.insert(
            name.clone(),
            Schema {
                name,
                owner: row.get("owner"),
                comment: row.get("comment"),
                acl: acl(row, "acl")?,
                ..Schema::default()
            },
        );
    }
    let oids: Vec<u32> = cat.namespaces.keys().copied().collect();
    let s = Some(oids.as_slice());
    let roles = async {
        if scope.manage_roles {
            query(client, "roles", &q.roles, None).await
        } else {
            Ok(Vec::new())
        }
    };
    let memberships = async {
        if scope.manage_roles {
            query(client, "role memberships", &q.memberships, None).await
        } else {
            Ok(Vec::new())
        }
    };
    let (
        relations,
        columns,
        indexes,
        constraints,
        triggers,
        policies,
        functions,
        types,
        derived,
        sequences,
        parts,
        extensions,
        event_triggers,
        depends,
        members,
        policy_roles,
        roles,
        memberships,
    ) = futures_util::try_join!(
        query(client, "relations", &q.relations, s),
        query(client, "columns", &q.columns, s),
        query(client, "indexes", &q.indexes, s),
        query(client, "constraints", &q.constraints, s),
        query(client, "triggers", &q.triggers, s),
        query(client, "policies", &q.policies, s),
        query(client, "functions", &q.functions, s),
        query(client, "types", &q.types, s),
        query(client, "derived types", &q.derived_types, s),
        query(client, "sequences", &q.sequences, s),
        query(client, "rules and defaults", &q.rules_and_defaults, s),
        query(client, "extensions", &q.extensions, None),
        query(client, "event triggers", &q.event_triggers, None),
        query(client, "dependencies", &q.depends, None),
        query(client, "extension members", &q.extension_members, None),
        query(client, "policy roles", &q.policy_roles, None),
        roles,
        memberships,
    )?;

    types::types(&mut db, &mut cat, &types)?;
    let identities = types::sequences(&mut db, &mut cat, &sequences)?;
    relations::relations(&mut db, &mut cat, &relations)?;
    relations::columns(&mut db, &mut cat, &columns, &identities)?;
    let mut index_rows = relations::indexes(&cat, &indexes)?;
    relations::constraints(&mut db, &mut cat, &constraints, &mut index_rows)?;
    relations::attach_indexes(&mut db, &mut cat, index_rows);
    relations::triggers(&mut db, &mut cat, &triggers)?;
    relations::policies(&mut db, &mut cat, &policies)?;
    routines::functions(&mut db, &mut cat, &functions)?;
    cluster::extensions(&mut db, &mut cat, &extensions, scope);
    cluster::event_triggers(&mut db, &mut cat, &event_triggers);
    cluster::roles(&mut db, &roles, &memberships);
    deps::dependencies(
        &mut db,
        &mut cat,
        deps::Rows {
            derived: &derived,
            parts: &parts,
            depends: &depends,
            members: &members,
            policy_roles: &policy_roles,
        },
    );
    data::load(client, &mut db, scope).await?;
    crate::restrict::restrict(&mut db, scope);
    Ok(db)
}

/// Decode an ACL column produced by `sql::acl`.
fn acl(row: &Row, column: &str) -> Result<Acl, Error> {
    let Json(items): Json<Vec<(String, String, bool)>> = row.get(column);
    items
        .into_iter()
        .map(|(grantee, privilege, grantable)| {
            let privilege = Privilege::parse(&privilege)
                .ok_or_else(|| Error::Inspect(format!("unknown privilege `{privilege}`")))?;
            Ok(AclItem {
                grantee,
                privilege,
                grantable,
            })
        })
        .collect()
}

/// A single-character catalog code (`relkind`, `contype`, ...).
fn code(row: &Row, column: &str) -> char {
    let s: String = row.get(column);
    s.chars().next().unwrap_or('\0')
}

fn unexpected(what: &str, value: impl std::fmt::Display) -> Error {
    Error::Inspect(format!("unexpected {what} `{value}`"))
}
