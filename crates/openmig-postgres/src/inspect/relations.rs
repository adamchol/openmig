use super::{Catalog, RelKind, Relation, SystemCatalog, acl, code, text, unexpected};
use crate::model::{
    Check, Column, CompositeField, ConstraintTrigger, Database, DomainCheck, Exclusion,
    ExclusionElement, FkAction, FkMatch, ForeignKey, Generated, GeneratedKind, Identity,
    IdentityGeneration, Index, IndexKey, KeyExpr, MaterializedView, NullsOrder, PartitionOf,
    Policy, PolicyCommand, PrimaryKey, QualifiedName, ReplicaIdentity, Schema, SequenceOptions,
    Storage, Table, Trigger, TriggerEnabled, TriggerEvent, TriggerLevel, TriggerTiming, Unique,
    View, ViewCheckOption, ViewColumn,
};
use openmig_core::{Error, ObjectKind, ObjectRef};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::Row;
use tokio_postgres::types::Json;

fn schema_mut<'a>(db: &'a mut Database, name: &str) -> &'a mut Schema {
    db.schemas
        .get_mut(name)
        .expect("relations belong to inspected schemas")
}

fn table_mut<'a>(db: &'a mut Database, rel: &Relation) -> Option<&'a mut Table> {
    schema_mut(db, &rel.schema).tables.get_mut(&rel.name)
}

fn is_true(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "on" | "yes" | "1" | "t" | "y"
    )
}

/// Tables, views and materialized views.
pub(super) fn relations(db: &mut Database, cat: &mut Catalog, rows: &[Row]) -> Result<(), Error> {
    for row in rows {
        let nsp: u32 = row.get("nsp");
        let Some(schema_name) = cat.schema(nsp).map(str::to_owned) else {
            continue;
        };
        let schema = schema_mut(db, &schema_name);
        let name: String = row.get("name");
        let options = text::options(row.get("options"));
        let owner: String = row.get("owner");
        let comment: Option<String> = row.get("comment");
        let acl = acl(row, "acl")?;
        let kind = match code(row, "kind") {
            'r' | 'p' => {
                let parents: Vec<QualifiedName> = row
                    .get::<_, Option<Json<Vec<(String, String)>>>>("parents")
                    .map(|Json(p)| {
                        p.into_iter()
                            .map(|(s, n)| QualifiedName::new(s, n))
                            .collect()
                    })
                    .unwrap_or_default();
                let (partition_of, inherits) = if row.get("is_partition") {
                    let parent = parents.into_iter().next().ok_or_else(|| {
                        Error::Inspect(format!("partition {schema_name}.{name} has no parent"))
                    })?;
                    let bound = row.get("partbound");
                    (Some(PartitionOf { parent, bound }), Vec::new())
                } else {
                    (None, parents)
                };
                let replica_identity = match code(row, "replident") {
                    'd' => ReplicaIdentity::Default,
                    'f' => ReplicaIdentity::Full,
                    'n' => ReplicaIdentity::Nothing,
                    // The index name is filled in by `attach_indexes`.
                    'i' => ReplicaIdentity::Index(String::new()),
                    other => return Err(unexpected("relreplident", other)),
                };
                schema.tables.insert(
                    name.clone(),
                    Table {
                        name: name.clone(),
                        rls_enabled: row.get("rls"),
                        rls_forced: row.get("rls_forced"),
                        replica_identity,
                        unlogged: code(row, "persistence") == 'u',
                        storage_params: options,
                        partition_by: row.get("partkey"),
                        partition_of,
                        inherits,
                        comment,
                        owner,
                        acl,
                        ..Table::default()
                    },
                );
                RelKind::Table
            }
            'v' => {
                let flag = |key: &str| options.get(key).is_some_and(|v| is_true(v));
                let check_option = match options.get("check_option").map(String::as_str) {
                    Some("local") => Some(ViewCheckOption::Local),
                    Some("cascaded") => Some(ViewCheckOption::Cascaded),
                    None => None,
                    Some(other) => return Err(unexpected("check_option", other)),
                };
                schema.views.insert(
                    name.clone(),
                    View {
                        name: name.clone(),
                        query: row.get("query"),
                        security_invoker: flag("security_invoker"),
                        security_barrier: flag("security_barrier"),
                        check_option,
                        comment,
                        owner,
                        acl,
                        ..View::default()
                    },
                );
                RelKind::View
            }
            'm' => {
                schema.materialized_views.insert(
                    name.clone(),
                    MaterializedView {
                        name: name.clone(),
                        query: row.get("query"),
                        with_data: row.get("populated"),
                        storage_params: options,
                        comment,
                        owner,
                        acl,
                        ..MaterializedView::default()
                    },
                );
                RelKind::MaterializedView
            }
            other => return Err(unexpected("relkind", other)),
        };
        cat.relations.insert(
            row.get("oid"),
            Relation {
                schema: schema_name,
                name,
                kind,
            },
        );
    }
    Ok(())
}

/// Columns of tables, views and materialized views, and fields of composite types.
pub(super) fn columns(
    db: &mut Database,
    cat: &mut Catalog,
    rows: &[Row],
    identities: &BTreeMap<(u32, i16), SequenceOptions>,
) -> Result<(), Error> {
    for row in rows {
        let rel: u32 = row.get("rel");
        let num: i16 = row.get("num");
        let Some(relation) = cat.relations.get(&rel) else {
            continue;
        };
        let name: String = row.get("name");
        cat.columns.insert((rel, num), name.clone());
        let schema = schema_mut(db, &relation.schema);
        let data_type: String = row.get("type");
        let comment: Option<String> = row.get("comment");
        match relation.kind {
            RelKind::Table => {
                let identity = match code(row, "identity") {
                    'a' => Some(IdentityGeneration::Always),
                    'd' => Some(IdentityGeneration::ByDefault),
                    _ => None,
                };
                let identity = identity
                    .map(|generation| {
                        let sequence = identities.get(&(rel, num)).copied().ok_or_else(|| {
                            Error::Inspect(format!(
                                "identity sequence of {}.{}.{name} not found",
                                relation.schema, relation.name
                            ))
                        })?;
                        Ok::<_, Error>(Identity {
                            generation,
                            sequence,
                        })
                    })
                    .transpose()?;
                let default: Option<String> = row.get("default");
                let (default, generated) = match code(row, "generated") {
                    's' => (None, Some(GeneratedKind::Stored)),
                    'v' => (None, Some(GeneratedKind::Virtual)),
                    _ => (default.clone(), None),
                };
                let generated = generated.map(|kind| Generated {
                    expr: row.get::<_, Option<String>>("default").unwrap_or_default(),
                    kind,
                });
                let storage = match row.get::<_, Option<String>>("storage").as_deref() {
                    None => None,
                    Some("p") => Some(Storage::Plain),
                    Some("e") => Some(Storage::External),
                    Some("x") => Some(Storage::Extended),
                    Some("m") => Some(Storage::Main),
                    Some(other) => return Err(unexpected("attstorage", other)),
                };
                let compression = match row.get::<_, String>("compression").as_str() {
                    "p" => Some("pglz".to_owned()),
                    "l" => Some("lz4".to_owned()),
                    _ => None,
                };
                let column = Column {
                    name,
                    data_type,
                    not_null: row.get("not_null"),
                    default,
                    identity,
                    generated,
                    collation: row.get("collation"),
                    storage,
                    compression,
                    comment,
                    acl: acl(row, "acl")?,
                };
                if let Some(t) = schema.tables.get_mut(&relation.name) {
                    t.columns.push(column);
                }
            }
            RelKind::View | RelKind::MaterializedView => {
                let column = ViewColumn {
                    name,
                    data_type,
                    comment,
                    acl: acl(row, "acl")?,
                };
                let columns = if relation.kind == RelKind::View {
                    schema.views.get_mut(&relation.name).map(|v| &mut v.columns)
                } else {
                    schema
                        .materialized_views
                        .get_mut(&relation.name)
                        .map(|v| &mut v.columns)
                };
                if let Some(columns) = columns {
                    columns.push(column);
                }
            }
            RelKind::Composite => {
                if let Some(c) = schema.composites.get_mut(&relation.name) {
                    c.fields.push(CompositeField {
                        name,
                        data_type,
                        collation: row.get("collation"),
                    });
                }
            }
            RelKind::Sequence | RelKind::IdentitySequence => {}
        }
    }
    Ok(())
}

/// An index row before it is attached to its table or constraint.
pub(super) struct IndexRow {
    rel: u32,
    index: Index,
    replident: bool,
}

#[derive(Deserialize)]
struct KeyRow {
    column: Option<String>,
    expr: Option<String>,
    key: bool,
    opclass: Option<String>,
    collation: Option<String>,
    desc: bool,
    nulls_first: bool,
}

pub(super) fn indexes(cat: &Catalog, rows: &[Row]) -> Result<BTreeMap<u32, IndexRow>, Error> {
    let mut out = BTreeMap::new();
    for row in rows {
        let rel: u32 = row.get("rel");
        if !cat.relations.contains_key(&rel) {
            continue;
        }
        let name: String = row.get("name");
        let parts = row
            .get::<_, Option<Json<Vec<KeyRow>>>>("keys")
            .map(|Json(k)| k)
            .unwrap_or_default();
        let mut keys = Vec::new();
        let mut include = Vec::new();
        for part in parts {
            let expr = match (part.column, part.expr) {
                (Some(c), _) => KeyExpr::Column(c),
                (None, Some(e)) => KeyExpr::Expression(e),
                (None, None) => {
                    return Err(Error::Inspect(format!("index {name} has an empty key")));
                }
            };
            if !part.key {
                if let KeyExpr::Column(c) = expr {
                    include.push(c);
                }
                continue;
            }
            let nulls = match (part.desc, part.nulls_first) {
                (false, true) => Some(NullsOrder::First),
                (true, false) => Some(NullsOrder::Last),
                _ => None,
            };
            keys.push(IndexKey {
                expr,
                opclass: part.opclass,
                collation: part.collation,
                descending: part.desc,
                nulls,
            });
        }
        let nulls_not_distinct: bool = row.get("nulls_not_distinct");
        out.insert(
            row.get("oid"),
            IndexRow {
                rel,
                replident: row.get("replident"),
                index: Index {
                    name,
                    method: row.get("method"),
                    unique: row.get("is_unique"),
                    keys,
                    include,
                    predicate: row.get("predicate"),
                    nulls_distinct: !nulls_not_distinct,
                    storage_params: text::options(row.get("options")),
                    comment: row.get("comment"),
                    constraint: false,
                },
            },
        );
    }
    Ok(out)
}

fn fk_action(c: char) -> Result<FkAction, Error> {
    Ok(match c {
        'a' => FkAction::NoAction,
        'r' => FkAction::Restrict,
        'c' => FkAction::Cascade,
        'n' => FkAction::SetNull,
        'd' => FkAction::SetDefault,
        other => return Err(unexpected("foreign key action", other)),
    })
}

/// Table and domain constraints. Indexes that implement a constraint are marked and lend it their
/// key details.
pub(super) fn constraints(
    db: &mut Database,
    cat: &mut Catalog,
    rows: &[Row],
    indexes: &mut BTreeMap<u32, IndexRow>,
) -> Result<(), Error> {
    for row in rows {
        let oid: u32 = row.get("oid");
        let name: String = row.get("name");
        let contype = code(row, "type");
        let comment: Option<String> = row.get("comment");
        let validated: bool = row.get("validated");
        let typ: u32 = row.get("typ");
        if typ != 0 {
            let Some(domain_ref) = cat.objects.get(&(SystemCatalog::Type, typ)).cloned() else {
                continue;
            };
            if domain_ref.kind != ObjectKind::Domain || contype != 'c' {
                continue;
            }
            let (schema, domain) = (&domain_ref.path[0], &domain_ref.path[1]);
            if let Some(d) = schema_mut(db, schema).domains.get_mut(domain) {
                d.checks.insert(
                    name.clone(),
                    DomainCheck {
                        name: name.clone(),
                        expr: row.get("expr"),
                        validated,
                        comment,
                    },
                );
            }
            cat.objects.insert(
                (SystemCatalog::Constraint, oid),
                ObjectRef::new(ObjectKind::Check, [schema, domain, &name]),
            );
            continue;
        }
        let rel: u32 = row.get("rel");
        let Some(relation) = cat.relations.get(&rel).cloned() else {
            continue;
        };
        let Some(table) = table_mut(db, &relation) else {
            continue;
        };
        let deferrable: bool = row.get("deferrable");
        let initially_deferred: bool = row.get("deferred");
        let index_oid: u32 = row.get("index");
        let mut backing = || {
            indexes.get_mut(&index_oid).map(|i| {
                i.index.constraint = true;
                i.index.clone()
            })
        };
        let kind = match contype {
            'p' => {
                let index = backing().unwrap_or_default();
                table.primary_key = Some(PrimaryKey {
                    name: name.clone(),
                    columns: row.get("columns"),
                    include: index.include,
                    deferrable,
                    initially_deferred,
                    storage_params: index.storage_params,
                    comment,
                });
                ObjectKind::PrimaryKey
            }
            'u' => {
                let index = backing().unwrap_or_default();
                table.uniques.insert(
                    name.clone(),
                    Unique {
                        name: name.clone(),
                        columns: row.get("columns"),
                        include: index.include,
                        nulls_distinct: index.nulls_distinct,
                        deferrable,
                        initially_deferred,
                        storage_params: index.storage_params,
                        comment,
                    },
                );
                ObjectKind::Unique
            }
            'f' => {
                let match_type = match code(row, "match") {
                    's' => FkMatch::Simple,
                    'f' => FkMatch::Full,
                    'p' => FkMatch::Partial,
                    other => return Err(unexpected("foreign key match type", other)),
                };
                table.foreign_keys.insert(
                    name.clone(),
                    ForeignKey {
                        name: name.clone(),
                        columns: row.get("columns"),
                        ref_table: QualifiedName::new(
                            row.get::<_, String>("ref_schema"),
                            row.get::<_, String>("ref_table"),
                        ),
                        ref_columns: row.get("ref_columns"),
                        on_update: fk_action(code(row, "on_update"))?,
                        on_delete: fk_action(code(row, "on_delete"))?,
                        match_type,
                        deferrable,
                        initially_deferred,
                        validated,
                        enforced: row.get("enforced"),
                        comment,
                    },
                );
                ObjectKind::ForeignKey
            }
            'c' => {
                table.checks.insert(
                    name.clone(),
                    Check {
                        name: name.clone(),
                        expr: row.get("expr"),
                        no_inherit: row.get("no_inherit"),
                        validated,
                        enforced: row.get("enforced"),
                        comment,
                    },
                );
                ObjectKind::Check
            }
            'x' => {
                let index = backing().unwrap_or_default();
                let operators: Vec<String> = row.get("operators");
                table.exclusions.insert(
                    name.clone(),
                    Exclusion {
                        name: name.clone(),
                        method: index.method,
                        elements: index
                            .keys
                            .into_iter()
                            .zip(operators)
                            .map(|(key, operator)| ExclusionElement { key, operator })
                            .collect(),
                        include: index.include,
                        predicate: index.predicate,
                        deferrable,
                        initially_deferred,
                        storage_params: index.storage_params,
                        comment,
                    },
                );
                ObjectKind::Exclusion
            }
            other => return Err(unexpected("contype", other)),
        };
        let r = ObjectRef::new(kind, [&relation.schema, &relation.name, &name]);
        if index_oid != 0 && matches!(contype, 'p' | 'u' | 'x') {
            cat.objects
                .insert((SystemCatalog::Class, index_oid), r.clone());
        }
        cat.objects.insert((SystemCatalog::Constraint, oid), r);
    }
    Ok(())
}

/// Put indexes into their tables and materialized views and resolve replica identity indexes.
pub(super) fn attach_indexes(
    db: &mut Database,
    cat: &mut Catalog,
    indexes: BTreeMap<u32, IndexRow>,
) {
    for (oid, row) in indexes {
        let Some(relation) = cat.relations.get(&row.rel) else {
            continue;
        };
        let name = row.index.name.clone();
        let schema = schema_mut(db, &relation.schema);
        let target = match relation.kind {
            RelKind::Table => schema.tables.get_mut(&relation.name).map(|t| {
                if row.replident && matches!(t.replica_identity, ReplicaIdentity::Index(_)) {
                    t.replica_identity = ReplicaIdentity::Index(name.clone());
                }
                &mut t.indexes
            }),
            RelKind::MaterializedView => schema
                .materialized_views
                .get_mut(&relation.name)
                .map(|m| &mut m.indexes),
            _ => None,
        };
        let Some(target) = target else {
            continue;
        };
        if !row.index.constraint {
            cat.objects.insert(
                (SystemCatalog::Class, oid),
                ObjectRef::new(ObjectKind::Index, [&relation.schema, &relation.name, &name]),
            );
        }
        target.insert(name, row.index);
    }
    for schema in db.schemas.values_mut() {
        for table in schema.tables.values_mut() {
            if table.replica_identity == ReplicaIdentity::Index(String::new()) {
                table.replica_identity = ReplicaIdentity::Nothing;
            }
        }
    }
}

pub(super) fn triggers(db: &mut Database, cat: &mut Catalog, rows: &[Row]) -> Result<(), Error> {
    for row in rows {
        let rel: u32 = row.get("rel");
        let Some(relation) = cat.relations.get(&rel) else {
            continue;
        };
        let name: String = row.get("name");
        let tgtype: i32 = row.get("type");
        let timing = if tgtype & (1 << 1) != 0 {
            TriggerTiming::Before
        } else if tgtype & (1 << 6) != 0 {
            TriggerTiming::InsteadOf
        } else {
            TriggerTiming::After
        };
        let mut events = BTreeSet::new();
        for (bit, event) in [
            (2, TriggerEvent::Insert),
            (3, TriggerEvent::Delete),
            (4, TriggerEvent::Update),
            (5, TriggerEvent::Truncate),
        ] {
            if tgtype & (1 << bit) != 0 {
                events.insert(event);
            }
        }
        let when = match row.get::<_, Option<String>>("def") {
            Some(def) => Some(text::trigger_when(&def).ok_or_else(|| {
                Error::Inspect(format!("cannot find the WHEN condition in `{def}`"))
            })?),
            None => None,
        };
        let nargs: i32 = row.get("nargs");
        let args = text::trigger_args(
            row.get::<_, &[u8]>("args"),
            usize::try_from(nargs).unwrap_or_default(),
        );
        let enabled = match code(row, "enabled") {
            'O' => TriggerEnabled::Origin,
            'D' => TriggerEnabled::Disabled,
            'R' => TriggerEnabled::Replica,
            'A' => TriggerEnabled::Always,
            other => return Err(unexpected("tgenabled", other)),
        };
        let constraint = row
            .get::<_, bool>("is_constraint")
            .then(|| ConstraintTrigger {
                deferrable: row.get("deferrable"),
                initially_deferred: row.get("deferred"),
                from: match (
                    row.get::<_, Option<String>>("from_schema"),
                    row.get::<_, Option<String>>("from_table"),
                ) {
                    (Some(s), Some(t)) => Some(QualifiedName::new(s, t)),
                    _ => None,
                },
            });
        let trigger = Trigger {
            name: name.clone(),
            timing,
            events,
            update_of: row.get("update_of"),
            level: if tgtype & 1 != 0 {
                TriggerLevel::Row
            } else {
                TriggerLevel::Statement
            },
            when,
            function: QualifiedName::new(
                row.get::<_, String>("func_schema"),
                row.get::<_, String>("func_name"),
            ),
            args,
            enabled,
            constraint,
            old_table: row.get("old_table"),
            new_table: row.get("new_table"),
            comment: row.get("comment"),
        };
        let schema = schema_mut(db, &relation.schema);
        let target = match relation.kind {
            RelKind::Table => schema
                .tables
                .get_mut(&relation.name)
                .map(|t| &mut t.triggers),
            RelKind::View => schema
                .views
                .get_mut(&relation.name)
                .map(|v| &mut v.triggers),
            _ => None,
        };
        if let Some(target) = target {
            target.insert(name.clone(), trigger);
            cat.objects.insert(
                (SystemCatalog::Trigger, row.get("oid")),
                ObjectRef::new(
                    ObjectKind::Trigger,
                    [&relation.schema, &relation.name, &name],
                ),
            );
        }
    }
    Ok(())
}

pub(super) fn policies(db: &mut Database, cat: &mut Catalog, rows: &[Row]) -> Result<(), Error> {
    for row in rows {
        let rel: u32 = row.get("rel");
        let Some(relation) = cat.relations.get(&rel) else {
            continue;
        };
        let Some(table) = table_mut(db, relation) else {
            continue;
        };
        let name: String = row.get("name");
        let command = match code(row, "cmd") {
            '*' => PolicyCommand::All,
            'r' => PolicyCommand::Select,
            'a' => PolicyCommand::Insert,
            'w' => PolicyCommand::Update,
            'd' => PolicyCommand::Delete,
            other => return Err(unexpected("polcmd", other)),
        };
        let mut roles: Vec<String> = row.get("roles");
        roles.sort();
        table.policies.insert(
            name.clone(),
            Policy {
                name: name.clone(),
                command,
                permissive: row.get("permissive"),
                roles,
                using: row.get("using"),
                check: row.get("check"),
                comment: row.get("comment"),
            },
        );
        cat.objects.insert(
            (SystemCatalog::Policy, row.get("oid")),
            ObjectRef::new(
                ObjectKind::Policy,
                [&relation.schema, &relation.name, &name],
            ),
        );
    }
    Ok(())
}
