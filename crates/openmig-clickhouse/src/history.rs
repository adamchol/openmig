//! The migration history table and the migration lock (see `docs/spec/formats.md`).
//!
//! * History: `[Replicated]ReplacingMergeTree(updated_at)` ordered by `version`. A write inserts
//!   a new version of the row with a strictly increasing `updated_at`; reads use `FINAL`.
//!   Replicated tables are written with a majority insert quorum and read with
//!   `select_sequential_consistency`, so a deploy reading through another replica still sees its
//!   own writes. Deletes are lightweight `DELETE FROM` statements, which ReplacingMergeTree
//!   tables support without an `is_deleted` column.
//! * Lock: a row in a `KeeperMap('/openmig/<history database>/lock')` table, inserted with
//!   `keeper_map_strict_mode = 1` (which fails when the row exists) and deleted on release. The
//!   row names its holder and when it was taken, so a lock left behind by a killed process is
//!   reported with its age. Without Keeper the lock is skipped with a warning.

use crate::session::ChSession;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use openmig_core::sql::{Dialect, quote_ident, quote_literal};
use openmig_core::{Error, ExecError, HistoryKind, HistoryRow, HistoryStatus};
use serde::Deserialize;
use std::time::{Duration, Instant};

const COMMENT: &str = "openmig-history v1";
const LOCK_TABLE: &str = "lock";
const LOCK_KEY: &str = "lock";
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Codes of errors that mean Keeper (or KeeperMap) is not available on the server.
const NO_KEEPER_CODES: &[&str] = &[
    "NO_ZOOKEEPER",
    "BAD_ARGUMENTS",
    "SUPPORT_IS_DISABLED",
    "UNKNOWN_STORAGE",
];

#[derive(Clone, Debug, Default)]
pub(crate) struct LockState {
    /// The `holder` value of the row this session inserted.
    held: Option<String>,
    /// Keeper is unavailable; locking is skipped.
    disabled: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryState {
    /// The history table's engine, read from the server once it exists.
    table: Option<TableKind>,
    /// The last `updated_at` written, in microseconds since the epoch.
    last_updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableKind {
    Local,
    /// A replicated table: writes need this insert quorum (a majority of its replicas).
    Replicated {
        quorum: u64,
    },
}

fn q(name: &str) -> String {
    quote_ident(name, Dialect::ClickHouse)
}

fn lit(value: &str) -> String {
    quote_literal(value, Dialect::ClickHouse)
}

fn history_sql(session: &ChSession) -> Result<(String, String, String), Error> {
    let history = session.settings.history()?;
    let table = history.to_sql();
    Ok((history.database, history.name, table))
}

async fn ensure_database(session: &ChSession, database: &str) -> Result<(), Error> {
    session
        .run(
            &format!(
                "CREATE DATABASE IF NOT EXISTS {}{}",
                q(database),
                session.settings.on_cluster()
            ),
            &[],
        )
        .await?;
    Ok(())
}

pub(crate) async fn ensure(session: &mut ChSession) -> Result<(), Error> {
    let (database, _, table) = history_sql(session)?;
    ensure_database(session, &database).await?;
    let replicated = session.settings.cluster.is_some();
    let engine = if replicated {
        "ReplicatedReplacingMergeTree"
    } else {
        "ReplacingMergeTree"
    };
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table}{on_cluster} (\
         `version` String, `name` String, `checksum` String, `kind` LowCardinality(String), \
         `transactional` Bool, `statements_total` UInt32, `statements_applied` UInt32, \
         `statement_checksums` Array(String), `status` LowCardinality(String), \
         `error` Nullable(String), `error_statement` Nullable(UInt32), \
         `started_at` DateTime64(3, 'UTC'), `finished_at` Nullable(DateTime64(3, 'UTC')), \
         `applied_by` String, `updated_at` DateTime64(6, 'UTC')\
         ) ENGINE = {engine}(`updated_at`) ORDER BY `version` COMMENT {comment}",
        on_cluster = session.settings.on_cluster(),
        comment = lit(COMMENT),
    );
    session.run(&sql, &[]).await?;
    session.history.table = None;
    Ok(())
}

/// The kind of the history table, or `None` when it does not exist.
async fn table_kind(session: &mut ChSession) -> Result<Option<TableKind>, Error> {
    if let Some(kind) = session.history.table {
        return Ok(Some(kind));
    }
    let (database, name, _) = history_sql(session)?;
    let (database, name) = (lit(&database), lit(&name));
    #[derive(Deserialize)]
    struct Engine {
        engine: String,
    }
    let rows: Vec<Engine> = session
        .http
        .rows(
            &format!(
                "SELECT engine FROM system.tables WHERE database = {database} AND name = {name}"
            ),
            &[],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let kind = if row.engine.starts_with("Replicated") {
        #[derive(Deserialize)]
        struct Replicas {
            total_replicas: u64,
        }
        let replicas: Vec<Replicas> = session
            .http
            .rows(
                &format!(
                    "SELECT total_replicas FROM system.replicas \
                     WHERE database = {database} AND table = {name}"
                ),
                &[],
            )
            .await?;
        let total = replicas.first().map_or(1, |r| r.total_replicas.max(1));
        TableKind::Replicated {
            quorum: total / 2 + 1,
        }
    } else {
        TableKind::Local
    };
    session.history.table = Some(kind);
    Ok(Some(kind))
}

#[derive(Deserialize)]
struct Row {
    version: String,
    name: String,
    checksum: String,
    kind: String,
    transactional: bool,
    statements_total: u32,
    statements_applied: u32,
    statement_checksums: Vec<String>,
    status: String,
    error: Option<String>,
    error_statement: Option<u32>,
    started_at: i64,
    finished_at: Option<i64>,
    applied_by: String,
}

pub(crate) async fn load(session: &mut ChSession) -> Result<Vec<HistoryRow>, Error> {
    let Some(kind) = table_kind(session).await? else {
        return Ok(Vec::new());
    };
    let (_, _, table) = history_sql(session)?;
    let settings: &[(&str, &str)] = match kind {
        TableKind::Replicated { .. } => &[("select_sequential_consistency", "1")],
        TableKind::Local => &[],
    };
    let rows: Vec<Row> = session
        .http
        .rows(
            &format!(
                "SELECT version, name, checksum, kind, transactional, statements_total, \
                 statements_applied, statement_checksums, status, error, error_statement, \
                 toUnixTimestamp64Milli(started_at) AS started_at, \
                 toUnixTimestamp64Milli(finished_at) AS finished_at, applied_by \
                 FROM {table} FINAL ORDER BY version"
            ),
            settings,
        )
        .await?;
    rows.into_iter().map(history_row).collect()
}

fn history_row(row: Row) -> Result<HistoryRow, Error> {
    let invalid = |what: &str, value: &str| {
        Error::Migrations(format!(
            "history row {} has an invalid {what} `{value}`",
            row.version
        ))
    };
    let timestamp = |ms: i64| {
        Timestamp::from_millisecond(ms).map_err(|e| invalid("timestamp", &format!("{ms} ({e})")))
    };
    Ok(HistoryRow {
        kind: HistoryKind::parse(&row.kind).ok_or_else(|| invalid("kind", &row.kind))?,
        status: HistoryStatus::parse(&row.status).ok_or_else(|| invalid("status", &row.status))?,
        started_at: timestamp(row.started_at)?,
        finished_at: row.finished_at.map(timestamp).transpose()?,
        name: row.name,
        checksum: row.checksum,
        transactional: row.transactional,
        statements_total: row.statements_total,
        statements_applied: row.statements_applied,
        statement_checksums: row.statement_checksums,
        error: row.error,
        error_statement: row.error_statement,
        applied_by: row.applied_by,
        version: row.version,
    })
}

pub(crate) async fn write(session: &mut ChSession, row: &HistoryRow) -> Result<(), Error> {
    let kind = table_kind(session).await?.ok_or_else(|| {
        Error::Migrations("the history table does not exist (history_ensure was not run)".into())
    })?;
    let (_, _, table) = history_sql(session)?;
    let now = Timestamp::now().as_microsecond();
    let updated_at = now.max(session.history.last_updated_at + 1);
    session.history.last_updated_at = updated_at;
    let opt = |v: Option<String>| v.unwrap_or_else(|| "NULL".to_owned());
    let checksums = row
        .statement_checksums
        .iter()
        .map(|c| lit(c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {table} (`version`, `name`, `checksum`, `kind`, `transactional`, \
         `statements_total`, `statements_applied`, `statement_checksums`, `status`, `error`, \
         `error_statement`, `started_at`, `finished_at`, `applied_by`, `updated_at`) VALUES \
         ({}, {}, {}, {}, {}, {}, {}, [{checksums}], {}, {}, {}, {}, {}, {}, {})",
        lit(&row.version),
        lit(&row.name),
        lit(&row.checksum),
        lit(row.kind.as_str()),
        row.transactional,
        row.statements_total,
        row.statements_applied,
        lit(row.status.as_str()),
        opt(row.error.as_deref().map(lit)),
        opt(row.error_statement.map(|n| n.to_string())),
        lit(&datetime(row.started_at.as_microsecond(), 3)),
        opt(row
            .finished_at
            .map(|t| lit(&datetime(t.as_microsecond(), 3)))),
        lit(&row.applied_by),
        lit(&datetime(updated_at, 6)),
    );
    let quorum;
    let mut settings = vec![("async_insert", "0")];
    if let TableKind::Replicated { quorum: q } = kind {
        // An explicit majority rather than `'auto'`, which never confirms on a single replica
        // (26.5). Sequential quorum inserts are what `select_sequential_consistency` relies on;
        // history writes are serialized by the migration lock anyway.
        quorum = q.to_string();
        settings.extend([
            ("insert_quorum", quorum.as_str()),
            ("insert_quorum_parallel", "0"),
        ]);
    }
    session.run(&sql, &settings).await?;
    Ok(())
}

pub(crate) async fn delete(session: &mut ChSession, version: &str) -> Result<(), Error> {
    if table_kind(session).await?.is_none() {
        return Ok(());
    }
    let (_, _, table) = history_sql(session)?;
    session
        .run(
            &format!("DELETE FROM {table} WHERE `version` = {}", lit(version)),
            &[("lightweight_deletes_sync", "2")],
        )
        .await?;
    Ok(())
}

/// `YYYY-MM-DD hh:mm:ss.f` in UTC with `digits` fractional digits (3 or 6).
fn datetime(micros: i64, digits: usize) -> String {
    let ts = Timestamp::from_microsecond(micros).unwrap_or(Timestamp::UNIX_EPOCH);
    let dt = ts.to_zoned(TimeZone::UTC).datetime();
    let fraction = format!("{:06}", dt.subsec_nanosecond() / 1000);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        &fraction[..digits.min(6)]
    )
}

fn holder(session: &ChSession) -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let nanos = Timestamp::now().as_nanosecond();
    format!(
        "openmig/{} host={host} pid={} user={} id={nanos:x}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        session.current_user
    )
}

fn lock_table(session: &ChSession) -> Result<(String, String), Error> {
    let database = session.settings.history()?.database;
    let table = format!("{}.{}", q(&database), q(LOCK_TABLE));
    Ok((database, table))
}

fn keeper_unavailable(error: &ExecError) -> bool {
    let message = error.message.to_lowercase();
    error
        .code
        .as_deref()
        .is_some_and(|c| NO_KEEPER_CODES.contains(&c))
        && (message.contains("keeper") || message.contains("zookeeper"))
}

fn already_held(error: &ExecError) -> bool {
    error.code.as_deref() == Some("KEEPER_EXCEPTION") && error.message.contains("Node exists")
}

pub(crate) async fn lock(session: &mut ChSession, wait: Duration) -> Result<(), Error> {
    if session.lock.held.is_some() || session.lock.disabled {
        return Ok(());
    }
    let (database, table) = lock_table(session)?;
    ensure_database(session, &database).await?;
    let create = format!(
        "CREATE TABLE IF NOT EXISTS {table}{} (`key` String, `holder` String, \
         `acquired_at` DateTime64(3, 'UTC')) ENGINE = KeeperMap({}) PRIMARY KEY `key`",
        session.settings.on_cluster(),
        lit(&format!("/openmig/{database}/lock")),
    );
    match session.run(&create, &[]).await {
        Ok(()) => {}
        Err(error) if keeper_unavailable(&error) => {
            tracing::warn!(
                %error,
                "ClickHouse Keeper is not available: deploying without a migration lock; make sure only one deploy runs at a time"
            );
            session.lock.disabled = true;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    let holder = holder(session);
    let insert = format!(
        "INSERT INTO {table} (`key`, `holder`, `acquired_at`) VALUES ({}, {}, now64(3))",
        lit(LOCK_KEY),
        lit(&holder)
    );
    let deadline = Instant::now() + wait;
    loop {
        match session
            .run(
                &insert,
                &[("keeper_map_strict_mode", "1"), ("async_insert", "0")],
            )
            .await
        {
            Ok(()) => {
                session.lock.held = Some(holder);
                return Ok(());
            }
            Err(error) if already_held(&error) => {
                if Instant::now() >= deadline {
                    return Err(Error::LockTimeout(
                        describe_holder(session, &table, wait).await,
                    ));
                }
                tokio::time::sleep(
                    LOCK_POLL.min(deadline.saturating_duration_since(Instant::now())),
                )
                .await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn describe_holder(session: &ChSession, table: &str, wait: Duration) -> String {
    #[derive(Deserialize)]
    struct Holder {
        holder: String,
        acquired_at: i64,
    }
    let rows: Result<Vec<Holder>, _> = session
        .http
        .rows(
            &format!(
                "SELECT holder, toUnixTimestamp64Milli(acquired_at) AS acquired_at FROM {table} \
                 WHERE `key` = {}",
                lit(LOCK_KEY)
            ),
            &[],
        )
        .await;
    let waited = format!("waited {wait:?}");
    match rows.ok().and_then(|r| r.into_iter().next()) {
        Some(h) => {
            let since = Timestamp::from_millisecond(h.acquired_at).unwrap_or(Timestamp::UNIX_EPOCH);
            let age = Timestamp::now().as_second() - since.as_second();
            format!(
                "{waited}; held by `{}` since {since} ({age}s ago); if that deploy is no longer \
                 running, release the lock with: DELETE FROM {table} WHERE `key` = {}",
                h.holder,
                lit(LOCK_KEY)
            )
        }
        None => format!("{waited}; the holder released it meanwhile"),
    }
}

pub(crate) async fn unlock(session: &mut ChSession) -> Result<(), Error> {
    let Some(holder) = session.lock.held.take() else {
        return Ok(());
    };
    let (_, table) = lock_table(session)?;
    session
        .run(
            &format!(
                "DELETE FROM {table} WHERE `key` = {} AND `holder` = {}",
                lit(LOCK_KEY),
                lit(&holder)
            ),
            &[],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetimes() {
        let micros = 1_793_000_000_123_456;
        assert_eq!(datetime(micros, 3), "2026-10-26 07:33:20.123");
        assert_eq!(datetime(micros, 6), "2026-10-26 07:33:20.123456");
    }

    #[test]
    fn lock_errors() {
        let held = ExecError {
            code: Some("KEEPER_EXCEPTION".into()),
            message: "Transaction failed (Node exists): Op #0, path: /keeper_map_tables/openmig/openmig/lock/data/bG9jaw==".into(),
            ..ExecError::default()
        };
        assert!(already_held(&held));
        assert!(!keeper_unavailable(&held));
        let disabled = ExecError {
            code: Some("BAD_ARGUMENTS".into()),
            message: "KeeperMap is disabled because 'keeper_map_path_prefix' config is not defined"
                .into(),
            ..ExecError::default()
        };
        assert!(keeper_unavailable(&disabled));
        let no_keeper = ExecError {
            code: Some("NO_ZOOKEEPER".into()),
            message: "Cannot use ZooKeeper: it is not configured".into(),
            ..ExecError::default()
        };
        assert!(keeper_unavailable(&no_keeper));
        let other = ExecError {
            code: Some("ACCESS_DENIED".into()),
            message: "Not enough privileges".into(),
            ..ExecError::default()
        };
        assert!(!keeper_unavailable(&other));
    }
}
