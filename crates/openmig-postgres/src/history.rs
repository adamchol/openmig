//! The migration history table (see `docs/spec/formats.md`).

use crate::conn::exec_error;
use crate::model::QualifiedName;
use jiff::Timestamp;
use openmig_core::{Error, HistoryKind, HistoryRow, HistoryStatus};
use tokio_postgres::Client;

const COMMENT: &str = "openmig-history v1";

const COLUMNS: &str = "\"version\", \"name\", \"checksum\", \"kind\", \"transactional\", \
    \"statements_total\", \"statements_applied\", \"statement_checksums\", \"status\", \"error\", \
    \"error_statement\", \"started_at\", \"finished_at\", \"applied_by\"";

fn create_sql(table: &QualifiedName) -> String {
    let t = table.to_sql();
    format!(
        "CREATE TABLE IF NOT EXISTS {t} (
  \"version\"             text PRIMARY KEY,
  \"name\"                text NOT NULL DEFAULT '',
  \"checksum\"            text NOT NULL,
  \"kind\"                text NOT NULL,
  \"transactional\"       boolean NOT NULL,
  \"statements_total\"    integer NOT NULL,
  \"statements_applied\"  integer NOT NULL,
  \"statement_checksums\" text[] NOT NULL DEFAULT '{{}}',
  \"status\"              text NOT NULL,
  \"error\"               text,
  \"error_statement\"     integer,
  \"started_at\"          timestamptz NOT NULL,
  \"finished_at\"         timestamptz,
  \"applied_by\"          text NOT NULL
);
COMMENT ON TABLE {t} IS '{COMMENT}'"
    )
}

async fn exists(client: &Client, table: &QualifiedName) -> Result<bool, Error> {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2)",
            &[&table.schema, &table.name],
        )
        .await
        .map_err(|e| exec_error(&e))?;
    Ok(row.get(0))
}

pub(crate) async fn ensure(client: &Client, table: &QualifiedName) -> Result<(), Error> {
    if exists(client, table).await? {
        return Ok(());
    }
    let schema_exists: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
            &[&table.schema],
        )
        .await
        .map_err(|e| exec_error(&e))?
        .get(0);
    let mut sql = String::new();
    if !schema_exists {
        sql.push_str(&format!(
            "CREATE SCHEMA IF NOT EXISTS {};\n",
            openmig_core::sql::quote_ident(&table.schema, openmig_core::Dialect::Postgres)
        ));
    }
    sql.push_str(&create_sql(table));
    client
        .batch_execute(&sql)
        .await
        .map_err(|e| Error::Exec(exec_error(&e).with_statement(sql.clone())))
}

pub(crate) async fn load(client: &Client, table: &QualifiedName) -> Result<Vec<HistoryRow>, Error> {
    if !exists(client, table).await? {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT \"version\", \"name\", \"checksum\", \"kind\", \"transactional\", \
         \"statements_total\", \"statements_applied\", \"statement_checksums\", \"status\", \
         \"error\", \"error_statement\", \
         (extract(epoch FROM \"started_at\") * 1000000)::int8, \
         (extract(epoch FROM \"finished_at\") * 1000000)::int8, \"applied_by\" \
         FROM {} ORDER BY \"version\" COLLATE \"C\"",
        table.to_sql()
    );
    let rows = client.query(&sql, &[]).await.map_err(|e| exec_error(&e))?;
    rows.iter()
        .map(|r| {
            let kind: String = r.get(3);
            let status: String = r.get(8);
            let version: String = r.get(0);
            let bad = |what: &str, value: &str| {
                Error::Migrations(format!(
                    "history row {version}: invalid {what} `{value}` in {}",
                    table
                ))
            };
            Ok(HistoryRow {
                name: r.get(1),
                checksum: r.get(2),
                kind: HistoryKind::parse(&kind).ok_or_else(|| bad("kind", &kind))?,
                transactional: r.get(4),
                statements_total: to_u32(r.get(5)),
                statements_applied: to_u32(r.get(6)),
                statement_checksums: r.get(7),
                status: HistoryStatus::parse(&status).ok_or_else(|| bad("status", &status))?,
                error: r.get(9),
                error_statement: r.get::<_, Option<i32>>(10).map(to_u32),
                started_at: micros(r.get(11)).ok_or_else(|| bad("started_at", ""))?,
                finished_at: r.get::<_, Option<i64>>(12).and_then(micros),
                applied_by: r.get(13),
                version,
            })
        })
        .collect()
}

fn to_u32(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn micros(v: i64) -> Option<Timestamp> {
    Timestamp::from_microsecond(v).ok()
}

pub(crate) async fn write(
    client: &Client,
    table: &QualifiedName,
    row: &HistoryRow,
) -> Result<(), Error> {
    let sql = format!(
        "INSERT INTO {} ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12::text::timestamptz, $13::text::timestamptz, $14) \
         ON CONFLICT (\"version\") DO UPDATE SET \"name\" = EXCLUDED.\"name\", \
         \"checksum\" = EXCLUDED.\"checksum\", \"kind\" = EXCLUDED.\"kind\", \
         \"transactional\" = EXCLUDED.\"transactional\", \
         \"statements_total\" = EXCLUDED.\"statements_total\", \
         \"statements_applied\" = EXCLUDED.\"statements_applied\", \
         \"statement_checksums\" = EXCLUDED.\"statement_checksums\", \
         \"status\" = EXCLUDED.\"status\", \"error\" = EXCLUDED.\"error\", \
         \"error_statement\" = EXCLUDED.\"error_statement\", \
         \"started_at\" = EXCLUDED.\"started_at\", \"finished_at\" = EXCLUDED.\"finished_at\", \
         \"applied_by\" = EXCLUDED.\"applied_by\"",
        table.to_sql()
    );
    let started_at = row.started_at.to_string();
    let finished_at = row.finished_at.map(|t| t.to_string());
    client
        .execute(
            &sql,
            &[
                &row.version,
                &row.name,
                &row.checksum,
                &row.kind.as_str(),
                &row.transactional,
                &to_i32(row.statements_total),
                &to_i32(row.statements_applied),
                &row.statement_checksums,
                &row.status.as_str(),
                &row.error,
                &row.error_statement.map(to_i32),
                &started_at,
                &finished_at,
                &row.applied_by,
            ],
        )
        .await
        .map_err(|e| exec_error(&e))?;
    Ok(())
}

pub(crate) async fn delete(
    client: &Client,
    table: &QualifiedName,
    version: &str,
) -> Result<(), Error> {
    let sql = format!("DELETE FROM {} WHERE \"version\" = $1", table.to_sql());
    client
        .execute(&sql, &[&version])
        .await
        .map_err(|e| exec_error(&e))?;
    Ok(())
}
