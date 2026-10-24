//! Throwaway database servers used to normalize schemas.
//!
//! OpenMig never asks users for a "dev database". Commands that need one start a private server
//! from the binaries on `PATH` (or a configured directory), use it, and stop it:
//!
//! * PostgreSQL: `initdb` into a temporary directory (a pristine data directory is cached under
//!   `$XDG_CACHE_HOME/openmig` and copied), then `postgres` listening only on a unix socket in that
//!   directory with durability turned off. `initdb` and `postgres` refuse to run as root, so when
//!   OpenMig runs as root the server runs as an unprivileged user (`run_as`, default `nobody`).
//! * ClickHouse: one `clickhouse server` with a generated config: embedded Keeper, a single-node
//!   cluster with macros (so `Replicated` engines and `ON CLUSTER` work), `local_directory` and
//!   `replicated` user stores, the configured storage policies mapped to the default disk, and no
//!   system log tables.
//!
//! Servers are started in their own process group with a parent-death signal so they never outlive
//! OpenMig, and are killed on drop. Their directories live in `/tmp/openmig-{pg,ch}-*`; directories
//! of dead OpenMig processes are removed when the next server starts.

mod clickhouse;
mod http;
mod postgres;
mod process;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use clickhouse::ClickHouseServer;
pub use postgres::PostgresServer;

#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    #[error(
        "could not find `{binary}` (looked in {searched}); add it to PATH or set `scratch.bin_dir` (PostgreSQL) or `scratch.bin` (ClickHouse)"
    )]
    BinaryNotFound { binary: String, searched: String },
    #[error("scratch server version {found} does not match the configured version {expected}")]
    VersionMismatch { expected: u32, found: String },
    #[error("scratch server failed to start: {message}\n{log}")]
    StartFailed { message: String, log: String },
    #[error("scratch server did not become ready within {seconds}s\n{log}")]
    Timeout { seconds: u64, log: String },
    #[error("user `{0}` (scratch.run_as) does not exist")]
    UnknownUser(String),
    #[error("invalid scratch settings: {0}")]
    InvalidSpec(String),
    #[error("scratch query failed (HTTP {status}): {message}")]
    Query { status: u16, message: String },
    #[error("scratch io error: {0}")]
    Io(#[from] std::io::Error),
}

/// PostgreSQL scratch settings (`[databases.<name>.scratch]` for postgres databases).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PgSpec {
    /// Directory containing `initdb` and `postgres`. Default: search `PATH`.
    pub bin_dir: Option<PathBuf>,
    /// Expected server major version; an error is raised when the binaries differ.
    #[serde(skip)]
    pub version: Option<u32>,
    /// User to run the server as when OpenMig runs as root. Default `nobody`.
    pub run_as: Option<String>,
    /// Extra `initdb` arguments. Default `--encoding=UTF8 --locale=C.UTF-8`.
    pub initdb_args: Option<Vec<String>>,
    /// Extra server settings passed as `-c key=value`.
    pub settings: BTreeMap<String, String>,
}

/// ClickHouse scratch settings (`[databases.<name>.scratch]` for clickhouse databases).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ChSpec {
    /// Path to the `clickhouse` binary. Default: search `PATH`.
    pub bin: Option<PathBuf>,
    #[serde(skip)]
    pub version: Option<u32>,
    /// Cluster name to define (single node). Filled from the database's `cluster` setting when it
    /// is a plain name; `{cluster}` style macros resolve through `macros.cluster`. Default:
    /// `macros.cluster`, else `openmig`.
    pub cluster: Option<String>,
    /// Server macros. Defaults: `shard = "01"`, `replica = "r1"`, `cluster = <cluster>`.
    pub macros: BTreeMap<String, String>,
    /// Storage policy names that schemas reference; each is mapped to the default disk.
    pub storage_policies: Vec<String>,
    /// Settings added to the default profile, e.g. `allow_experimental_time_series_table = "1"`.
    pub profile_settings: BTreeMap<String, String>,
    /// Extra XML (or YAML) merged into the generated server config (`config.d`).
    pub extra_config: Option<PathBuf>,
    /// User to run the server as when OpenMig runs as root. Default: run as the current user.
    pub run_as: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    Postgres(PgSpec),
    ClickHouse(ChSpec),
}

/// A running scratch server.
pub enum ScratchServer {
    Postgres(PostgresServer),
    ClickHouse(ClickHouseServer),
}

impl ScratchServer {
    pub async fn start(spec: &Spec) -> Result<Self, ScratchError> {
        Ok(match spec {
            Spec::Postgres(s) => ScratchServer::Postgres(PostgresServer::start(s).await?),
            Spec::ClickHouse(s) => ScratchServer::ClickHouse(ClickHouseServer::start(s).await?),
        })
    }

    /// Connection URL for an administrative connection.
    ///
    /// PostgreSQL: `postgresql:///postgres?host=<socket dir>&port=<port>&user=openmig` (superuser,
    /// trust auth). ClickHouse: `http://default@127.0.0.1:<http port>/` (no password, full access).
    pub fn url(&self) -> &str {
        match self {
            ScratchServer::Postgres(s) => s.url(),
            ScratchServer::ClickHouse(s) => s.url(),
        }
    }

    /// Server version string as reported by the binary, e.g. `17.11`.
    pub fn version(&self) -> &str {
        match self {
            ScratchServer::Postgres(s) => s.version(),
            ScratchServer::ClickHouse(s) => s.version(),
        }
    }

    /// Stop the server and remove its files.
    pub async fn stop(self) -> Result<(), ScratchError> {
        match self {
            ScratchServer::Postgres(s) => s.stop().await,
            ScratchServer::ClickHouse(s) => s.stop().await,
        }
    }
}
