//! Databases, tables, views, materialized views, dictionaries and functions.

use super::{ColumnRow, Snapshot, TableRow};
use crate::ddl::{self, CreateKind, ParsedColumn, StorageClauses};
use crate::model::{
    Column, ColumnDefault, Constraint, ConstraintKind, Database, DefaultKind, Dictionary,
    DictionaryAttribute, Function, Index, InnerTable, MaterializedView, Model, Projection,
    QualifiedName, Table, TimeSeriesTargets, View,
};
use openmig_core::Error;
use std::collections::BTreeMap;

pub(super) fn assemble(model: &mut Model, snapshot: &Snapshot) -> Result<(), Error> {
    for row in &snapshot.databases {
        let mut db = Database::new(&row.name, &row.engine_full);
        if db.engine.is_empty() {
            db.engine.clone_from(&row.engine);
        }
        db.comment = non_empty(&row.comment);
        model.databases.insert(row.name.clone(), db);
    }

    let mut columns: BTreeMap<(&str, &str), Vec<&ColumnRow>> = BTreeMap::new();
    for c in &snapshot.columns {
        columns
            .entry((c.database.as_str(), c.table.as_str()))
            .or_default()
            .push(c);
    }

    for row in &snapshot.tables {
        let Some(db) = model.databases.get_mut(&row.database) else {
            continue;
        };
        let key = (row.database.as_str(), row.name.as_str());
        let table_columns = columns.get(&key).map_or(&[][..], Vec::as_slice);
        let fail = |e: ddl::DdlError| Error::Inspect(format!("{}.{}: {e}", row.database, row.name));
        match ddl::create_kind(&row.create_table_query) {
            CreateKind::Table => {
                let parsed = ddl::parse_table(&row.create_table_query).map_err(fail)?;
                let table = table(row, parsed, table_columns).map_err(fail)?;
                db.tables.insert(row.name.clone(), table);
            }
            CreateKind::View => {
                let parsed = ddl::parse_view(&row.create_table_query).map_err(fail)?;
                db.views.insert(
                    row.name.clone(),
                    View {
                        name: row.name.clone(),
                        query: row.as_select.clone(),
                        sql_security: parsed.sql_security,
                        definer: definer(parsed.definer, &snapshot.current_user),
                        comment: non_empty(&row.comment),
                    },
                );
            }
            CreateKind::MaterializedView => {
                let parsed = ddl::parse_view(&row.create_table_query).map_err(fail)?;
                let mv = MaterializedView {
                    name: row.name.clone(),
                    to: parsed.to.map(|to| QualifiedName {
                        database: to.database.unwrap_or_else(|| row.database.clone()),
                        name: to.name,
                    }),
                    query: row.as_select.clone(),
                    columns: merge_columns(table_columns, &parsed.columns).map_err(fail)?,
                    refresh: parsed.refresh,
                    inner: parsed.storage.map(inner_table),
                    sql_security: parsed.sql_security,
                    definer: definer(parsed.definer, &snapshot.current_user),
                    comment: non_empty(&row.comment),
                };
                db.materialized_views.insert(row.name.clone(), mv);
            }
            CreateKind::Dictionary => {
                let parsed = ddl::parse_dictionary(&row.create_table_query).map_err(fail)?;
                db.dictionaries
                    .insert(row.name.clone(), dictionary(row, parsed).map_err(fail)?);
            }
            CreateKind::Other => {
                tracing::warn!(
                    "skipping {}.{}: unsupported object: {}",
                    row.database,
                    row.name,
                    row.create_table_query
                );
            }
        }
    }

    for row in &snapshot.functions {
        let parsed = ddl::parse_function(&row.create_query)
            .map_err(|e| Error::Inspect(format!("function {}: {e}", row.name)))?;
        model.functions.insert(
            row.name.clone(),
            Function {
                name: row.name.clone(),
                parameters: parsed.parameters,
                body: parsed.body,
            },
        );
    }
    Ok(())
}

fn non_empty(text: &str) -> Option<String> {
    Some(text.to_owned()).filter(|t| !t.is_empty())
}

/// The definer unless it is the connecting user: objects created by whoever applies the schema
/// (the scratch server's `default` user, the deploying user) must compare equal.
fn definer(definer: Option<String>, current_user: &str) -> Option<String> {
    definer.filter(|d| d != current_user)
}

fn table(row: &TableRow, parsed: ddl::ParsedTable, columns: &[&ColumnRow]) -> ddl::Result<Table> {
    let storage = parsed.storage;
    let time_series = storage.engine == "TimeSeries";
    let mut table = Table {
        name: row.name.clone(),
        columns: if time_series {
            Vec::new()
        } else {
            merge_columns(columns, &parsed.elements.columns)?
        },
        comment: non_empty(&row.comment),
        ..Table::default()
    };
    if time_series {
        let target = |key: &str| storage.targets.get(key).cloned();
        table.time_series = Some(TimeSeriesTargets {
            data: target("DATA"),
            tags: target("TAGS"),
            metrics: target("METRICS"),
        });
    }
    let inner = inner_table(storage);
    table.engine = inner.engine;
    table.partition_by = inner.partition_by;
    table.order_by = inner.order_by;
    table.primary_key = inner.primary_key;
    table.sample_by = inner.sample_by;
    table.ttl = inner.ttl;
    table.settings = inner.settings;

    for index in parsed.elements.indexes {
        table.indexes.insert(
            index.name,
            Index {
                expression: index.expression,
                index_type: index.index_type,
                granularity: index.granularity.unwrap_or(1),
            },
        );
    }
    for p in parsed.elements.projections {
        table.projections.insert(
            p.name,
            Projection {
                query: p.query,
                settings: p.settings,
            },
        );
    }
    for c in parsed.elements.constraints {
        let kind = match c.kind.as_str() {
            "ASSUME" => ConstraintKind::Assume,
            _ => ConstraintKind::Check,
        };
        table.constraints.insert(
            c.name,
            Constraint {
                kind,
                expression: c.expression,
            },
        );
    }
    Ok(table)
}

fn inner_table(storage: StorageClauses) -> InnerTable {
    let order_by = storage.order_by.unwrap_or_default();
    InnerTable {
        engine: storage.engine,
        partition_by: storage.partition_by,
        primary_key: storage.primary_key.unwrap_or_else(|| order_by.clone()),
        order_by,
        sample_by: storage.sample_by,
        ttl: storage.ttl,
        settings: storage.settings,
    }
}

/// Columns from `system.columns`, completed with the clauses only the DDL shows (TTL,
/// statistics, column settings).
fn merge_columns(reported: &[&ColumnRow], parsed: &[ParsedColumn]) -> ddl::Result<Vec<Column>> {
    reported
        .iter()
        .map(|c| {
            let extra = parsed.iter().find(|p| p.name == c.name);
            let codec = match c
                .compression_codec
                .strip_prefix("CODEC(")
                .and_then(|s| s.strip_suffix(')'))
            {
                Some(list) => ddl::split_list(list)?,
                None => Vec::new(),
            };
            Ok(Column {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                default: DefaultKind::parse(&c.default_kind).map(|kind| ColumnDefault {
                    kind,
                    expression: ddl::normalize_expression(&c.default_expression),
                }),
                codec,
                ttl: extra.and_then(|p| p.ttl.clone()),
                statistics: extra.map(|p| p.statistics.clone()).unwrap_or_default(),
                settings: extra.map(|p| p.settings.clone()).unwrap_or_default(),
                comment: non_empty(&c.comment),
            })
        })
        .collect()
}

fn dictionary(row: &TableRow, parsed: ddl::ParsedDictionary) -> ddl::Result<Dictionary> {
    let missing = |what: &str| ddl::DdlError {
        message: format!("dictionary without {what}"),
        ddl: row.create_table_query.clone(),
    };
    Ok(Dictionary {
        name: row.name.clone(),
        attributes: parsed
            .attributes
            .into_iter()
            .map(|a| DictionaryAttribute {
                name: a.name,
                data_type: a.data_type,
                default: a.default,
                expression: a.expression,
                hierarchical: a.hierarchical,
                bidirectional: a.bidirectional,
                injective: a.injective,
                is_object_id: a.is_object_id,
            })
            .collect(),
        primary_key: parsed.primary_key,
        source: parsed.source.ok_or_else(|| missing("SOURCE"))?,
        lifetime: parsed.lifetime,
        layout: parsed.layout.ok_or_else(|| missing("LAYOUT"))?,
        range: parsed.range,
        settings: parsed.settings,
        comment: non_empty(&row.comment),
    })
}
