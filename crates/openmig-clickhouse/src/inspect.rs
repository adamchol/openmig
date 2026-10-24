//! Reading a server into the [`Model`].
//!
//! A handful of batch queries on the system tables (`databases`, `tables`, `columns`,
//! `functions`, `roles`, `users`, `grants`, `role_grants`, `privileges`, `row_policies`) read
//! everything at once; clauses the system tables do not expose are parsed from
//! `create_table_query` (see [`crate::ddl`]). Skip indexes come from the DDL too:
//! `system.data_skipping_indices` lists implicit indexes as well and prints a tuple expression
//! without its parentheses (`(a), (b)`), which is not valid in `ADD INDEX`. Expressions are
//! normalized with [`crate::ddl::normalize_expression`]. The result is restricted to the scope
//! with [`crate::restrict::restrict`].

mod access;
mod deps;
mod objects;

use crate::model::Model;
use crate::restrict::restrict;
use crate::session::ChSession;
use crate::settings::SYSTEM_DATABASES;
use openmig_core::sql::{Dialect, quote_literal};
use openmig_core::{Error, ExecError, SchemaFilter, Scope, Session};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Database engines whose tables are created with DDL (as opposed to engines that mirror
/// another system, such as `MySQL` or `PostgreSQL`, whose tables are not managed).
const DDL_DATABASE_ENGINES: &[&str] = &[
    "Atomic",
    "Ordinary",
    "Replicated",
    "Lazy",
    "Memory",
    "Shared",
];

#[derive(Deserialize)]
pub(crate) struct DatabaseRow {
    pub name: String,
    pub engine: String,
    pub engine_full: String,
    pub comment: String,
}

#[derive(Deserialize)]
pub(crate) struct TableRow {
    pub database: String,
    pub name: String,
    pub create_table_query: String,
    pub as_select: String,
    pub comment: String,
    pub dependencies_database: Vec<String>,
    pub dependencies_table: Vec<String>,
    pub loading_dependencies_database: Vec<String>,
    pub loading_dependencies_table: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct ColumnRow {
    pub database: String,
    pub table: String,
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    pub default_kind: String,
    pub default_expression: String,
    pub compression_codec: String,
    pub comment: String,
}

#[derive(Deserialize)]
pub(crate) struct FunctionRow {
    pub name: String,
    pub create_query: String,
}

/// Everything read from the system tables, before assembly.
pub(crate) struct Snapshot {
    pub current_user: String,
    pub databases: Vec<DatabaseRow>,
    pub tables: Vec<TableRow>,
    pub columns: Vec<ColumnRow>,
    pub functions: Vec<FunctionRow>,
    pub access: access::AccessRows,
}

pub(crate) async fn inspect(session: &mut ChSession, scope: &Scope) -> Result<Model, Error> {
    let snapshot = read(session, scope).await?;
    let catalog = session.privileges().await?.clone();
    let version = session.server_version();
    let mut model = Model {
        server_version: (version.major, version.minor),
        ..Model::default()
    };
    objects::assemble(&mut model, &snapshot)?;
    access::assemble(&mut model, &snapshot, &catalog);
    deps::assemble(&mut model, &snapshot);
    restrict(&mut model, scope);
    model.normalize();
    Ok(model)
}

async fn rows<T: DeserializeOwned>(session: &ChSession, sql: &str) -> Result<Vec<T>, Error> {
    session
        .http
        .rows(sql, &[])
        .await
        .map_err(|e: ExecError| Error::Inspect(format!("{e}\n  query: {sql}")))
}

fn list(values: &[String]) -> String {
    values
        .iter()
        .map(|v| quote_literal(v, Dialect::ClickHouse))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn read(session: &ChSession, scope: &Scope) -> Result<Snapshot, Error> {
    let history = session.settings.history()?;
    let mut hidden: Vec<String> = SYSTEM_DATABASES.iter().map(|d| (*d).to_owned()).collect();
    hidden.push(history.database.clone());
    let mut filter = format!("name NOT IN ({})", list(&hidden));
    if let SchemaFilter::Only(names) = &scope.schemas {
        if names.is_empty() {
            filter = "0".to_owned();
        } else {
            filter.push_str(&format!(" AND name IN ({})", list(names)));
        }
    }
    let databases: Vec<DatabaseRow> = rows(
        session,
        &format!(
            "SELECT name, engine, engine_full, comment FROM system.databases \
             WHERE {filter} ORDER BY name"
        ),
    )
    .await?;
    let with_tables: Vec<String> = databases
        .iter()
        .filter(|d| DDL_DATABASE_ENGINES.contains(&d.engine.as_str()))
        .map(|d| d.name.clone())
        .collect();

    let (mut tables, mut columns) = (Vec::new(), Vec::new());
    if !with_tables.is_empty() {
        let in_databases = list(&with_tables);
        tables = rows(
            session,
            &format!(
                "SELECT database, name, create_table_query, as_select, comment, \
                 dependencies_database, dependencies_table, loading_dependencies_database, \
                 loading_dependencies_table FROM system.tables \
                 WHERE database IN ({in_databases}) AND NOT is_temporary \
                 AND NOT startsWith(name, '.inner') ORDER BY database, name"
            ),
        )
        .await?;
        columns = rows(
            session,
            &format!(
                "SELECT database, table, name, type, default_kind, default_expression, \
                 compression_codec, comment FROM system.columns \
                 WHERE database IN ({in_databases}) AND NOT startsWith(table, '.inner') \
                 ORDER BY database, table, position"
            ),
        )
        .await?;
    }
    let functions = rows(
        session,
        "SELECT name, create_query FROM system.functions WHERE origin = 'SQLUserDefined' \
         ORDER BY name",
    )
    .await?;
    let access = access::read(session, &with_tables).await?;
    Ok(Snapshot {
        current_user: session.current_user.clone(),
        databases,
        tables,
        columns,
        functions,
        access,
    })
}
