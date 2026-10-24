//! Process-wide pools of scratch servers for integration tests.
//!
//! [`postgres`] and [`clickhouse`] hand out exclusive leases. Each pool lazily starts up to
//! `OPENMIG_TEST_PG_SERVERS` (default 2) PostgreSQL clusters and `OPENMIG_TEST_CH_SERVERS`
//! (default 1) ClickHouse servers, shared by every test of the test binary and every tokio runtime.
//! Dropping a lease returns its server; the next lease of a used server resets it first:
//!
//! * PostgreSQL (through `psql`): every database is dropped `WITH (FORCE)` and `postgres` is
//!   re-created, then every role except the bootstrap superuser and `pg_*` is dropped.
//! * ClickHouse (over HTTP): every non-system database (including `default`, which is re-created)
//!   is dropped `SYNC`, along with SQL-defined users, roles, row policies, quotas, settings
//!   profiles, functions and named collections.
//!
//! A server that cannot be reset is replaced by a new one. The pools live in statics and are never
//! dropped; servers die with the test process (parent-death signal) and their directories are
//! removed by the next scratch server start.

use openmig_scratch::{ChSpec, ClickHouseServer, PgSpec, PostgresServer, ScratchError};
use std::collections::BTreeMap;
use std::io::Write;
use std::ops::Deref;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use tokio::sync::{Semaphore, SemaphorePermit};

/// Exclusive use of a pooled PostgreSQL cluster. Derefs to [`PostgresServer`].
pub type PgLease = Lease<PostgresServer>;
/// Exclusive use of a pooled ClickHouse server. Derefs to [`ClickHouseServer`].
pub type ChLease = Lease<ClickHouseServer>;

/// Leases a PostgreSQL cluster (default [`PgSpec`]; major version from `OPENMIG_TEST_PG_VERSION`).
///
/// # Panics
///
/// When a server cannot be started.
pub async fn postgres() -> PgLease {
    static POOL: OnceLock<Pool<PostgresServer>> = OnceLock::new();
    acquire(POOL.get_or_init(|| Pool::new(pool_size("OPENMIG_TEST_PG_SERVERS", 2)))).await
}

/// Leases a ClickHouse server: cluster `openmig`, storage policy `encrypted`,
/// `allow_experimental_time_series_table = 1`.
///
/// # Panics
///
/// When a server cannot be started.
pub async fn clickhouse() -> ChLease {
    static POOL: OnceLock<Pool<ClickHouseServer>> = OnceLock::new();
    acquire(POOL.get_or_init(|| Pool::new(pool_size("OPENMIG_TEST_CH_SERVERS", 1)))).await
}

pub fn postgres_spec() -> PgSpec {
    PgSpec {
        version: std::env::var("OPENMIG_TEST_PG_VERSION").ok().map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("OPENMIG_TEST_PG_VERSION={v} is not a major version"))
        }),
        ..PgSpec::default()
    }
}

pub fn clickhouse_spec() -> ChSpec {
    ChSpec {
        cluster: Some("openmig".to_owned()),
        storage_policies: vec!["encrypted".to_owned()],
        profile_settings: BTreeMap::from([(
            "allow_experimental_time_series_table".to_owned(),
            "1".to_owned(),
        )]),
        ..ChSpec::default()
    }
}

fn pool_size(variable: &str, default: usize) -> usize {
    match std::env::var(variable) {
        Ok(value) => match value.parse() {
            Ok(size) if size > 0 => size,
            _ => panic!("{variable}={value} is not a positive number"),
        },
        Err(_) => default,
    }
}

struct Pool<S> {
    idle: Mutex<Vec<S>>,
    permits: Semaphore,
}

impl<S> Pool<S> {
    fn new(size: usize) -> Self {
        Pool {
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(size),
        }
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<S>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

trait Pooled: Sized + 'static {
    async fn start() -> Result<Self, ScratchError>;
    async fn reset(&self) -> Result<(), String>;
}

impl Pooled for PostgresServer {
    async fn start() -> Result<Self, ScratchError> {
        PostgresServer::start(&postgres_spec()).await
    }

    async fn reset(&self) -> Result<(), String> {
        psql(self, "template1", RESET_POSTGRES).await.map(drop)
    }
}

impl Pooled for ClickHouseServer {
    async fn start() -> Result<Self, ScratchError> {
        ClickHouseServer::start(&clickhouse_spec()).await
    }

    async fn reset(&self) -> Result<(), String> {
        reset_clickhouse(self)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Exclusive use of a pooled server until dropped.
pub struct Lease<S: 'static> {
    server: Option<S>,
    pool: &'static Pool<S>,
    _permit: SemaphorePermit<'static>,
}

impl<S> Deref for Lease<S> {
    type Target = S;

    fn deref(&self) -> &S {
        self.server
            .as_ref()
            .expect("a lease holds its server until dropped")
    }
}

impl<S> Drop for Lease<S> {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            self.pool.idle().push(server);
        }
    }
}

async fn acquire<S: Pooled>(pool: &'static Pool<S>) -> Lease<S> {
    let permit = pool
        .permits
        .acquire()
        .await
        .expect("the pool semaphore is never closed");
    let reused = pool.idle().pop();
    let server = match reused {
        Some(server) => match server.reset().await {
            Ok(()) => server,
            Err(error) => {
                eprintln!("could not reset a pooled scratch server, starting a new one: {error}");
                drop(server);
                start::<S>().await
            }
        },
        None => start::<S>().await,
    };
    Lease {
        server: Some(server),
        pool,
        _permit: permit,
    }
}

async fn start<S: Pooled>() -> S {
    S::start()
        .await
        .unwrap_or_else(|error| panic!("could not start a scratch server: {error}"))
}

impl PgLease {
    /// URL of `database` on this cluster, connecting as the superuser.
    pub fn database_url(&self, database: &str) -> String {
        database_url(self, database)
    }

    /// Creates an empty database and returns its URL.
    ///
    /// # Panics
    ///
    /// When the database cannot be created.
    pub async fn create_database(&self, name: &str) -> String {
        let sql = format!("CREATE DATABASE {}", quote_pg(name));
        if let Err(error) = psql(self, "postgres", &sql).await {
            panic!("{sql}: {error}");
        }
        self.database_url(name)
    }
}

const RESET_POSTGRES: &str = r"
SELECT format('DROP DATABASE %I WITH (FORCE)', datname) FROM pg_database
WHERE datname NOT IN ('template0', 'template1') ORDER BY datname \gexec
CREATE DATABASE postgres;
SELECT format('DROP OWNED BY %I CASCADE', rolname) FROM pg_roles
WHERE rolname <> current_user AND rolname !~ '^pg_' ORDER BY rolname \gexec
SELECT format('DROP ROLE %I', rolname) FROM pg_roles
WHERE rolname <> current_user AND rolname !~ '^pg_' ORDER BY rolname \gexec
";

fn database_url(server: &PostgresServer, database: &str) -> String {
    let url = server.url();
    let query = url.split_once('?').map_or("", |(_, query)| query);
    format!("postgresql:///{}?{query}", encode_path_segment(database))
}

fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn quote_pg(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_ch(identifier: &str) -> String {
    format!("`{}`", identifier.replace('\\', "\\\\").replace('`', "\\`"))
}

fn psql_binary(server: &PostgresServer) -> PathBuf {
    let next_to_server = server.bin_dir().join("psql");
    if next_to_server.is_file() {
        next_to_server
    } else {
        PathBuf::from("psql")
    }
}

/// Runs a psql script (meta-commands allowed) against `database` and returns its unaligned,
/// tuples-only output.
async fn psql(server: &PostgresServer, database: &str, script: &str) -> Result<String, String> {
    let mut cmd = Command::new(psql_binary(server));
    cmd.args(["-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-d"])
        .arg(database_url(server, database))
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let script = script.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut child = cmd.spawn()?;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(script.as_bytes())?;
        child.wait_with_output()
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("could not run psql: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_owned())
    }
}

async fn reset_clickhouse(server: &ClickHouseServer) -> Result<(), ScratchError> {
    async fn rows(server: &ClickHouseServer, sql: &str) -> Result<Vec<Vec<String>>, ScratchError> {
        let body = server.query(&format!("{sql} FORMAT TSVRaw")).await?;
        Ok(body
            .lines()
            .map(|line| line.split('\t').map(str::to_owned).collect())
            .collect())
    }

    for row in rows(
        server,
        "SELECT name FROM system.databases \
         WHERE name NOT IN ('system', 'information_schema', 'INFORMATION_SCHEMA') ORDER BY name",
    )
    .await?
    {
        server
            .query(&format!("DROP DATABASE {} SYNC", quote_ch(&row[0])))
            .await?;
    }
    server
        .query("CREATE DATABASE IF NOT EXISTS default")
        .await?;

    for row in rows(
        server,
        "SELECT short_name, database, table FROM system.row_policies \
         WHERE storage != 'users_xml' ORDER BY short_name, database, table",
    )
    .await?
    {
        server
            .query(&format!(
                "DROP ROW POLICY IF EXISTS {} ON {}.{}",
                quote_ch(&row[0]),
                quote_ch(&row[1]),
                quote_ch(&row[2])
            ))
            .await?;
    }
    for (table, kind) in [
        ("quotas", "QUOTA"),
        ("settings_profiles", "SETTINGS PROFILE"),
        ("users", "USER"),
        ("roles", "ROLE"),
    ] {
        for row in rows(
            server,
            &format!("SELECT name FROM system.{table} WHERE storage != 'users_xml' ORDER BY name"),
        )
        .await?
        {
            server
                .query(&format!("DROP {kind} IF EXISTS {}", quote_ch(&row[0])))
                .await?;
        }
    }
    for row in rows(
        server,
        "SELECT name FROM system.functions WHERE origin = 'SQLUserDefined' ORDER BY name",
    )
    .await?
    {
        server
            .query(&format!("DROP FUNCTION IF EXISTS {}", quote_ch(&row[0])))
            .await?;
    }
    for row in rows(
        server,
        "SELECT name FROM system.named_collections ORDER BY name",
    )
    .await?
    {
        server
            .query(&format!(
                "DROP NAMED COLLECTION IF EXISTS {}",
                quote_ch(&row[0])
            ))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_pg("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_ch("a`b\\c"), "`a\\`b\\\\c`");
        assert_eq!(encode_path_segment("app db/1"), "app%20db%2F1");
    }
}
