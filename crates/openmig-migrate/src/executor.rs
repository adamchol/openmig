//! Applying migration files to a database.

use crate::dir::{MigrationDir, MigrationFile};
use openmig_core::{Engine, Error, HistoryRow};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct DeployOptions {
    /// Stop after this version (inclusive).
    pub to: Option<String>,
    /// When history is empty: record every file up to and including this version as a baseline
    /// without running it.
    pub baseline: Option<String>,
    /// Allow a first deploy into a database that already has objects in scope.
    pub allow_dirty: bool,
    pub allow_out_of_order: bool,
    /// Print what would run without running it.
    pub dry_run: bool,
    /// Per-file lock timeout for transactional files.
    pub lock_timeout: Duration,
    /// How many times a transactional file is retried after a lock timeout.
    pub retries: u32,
    /// How long to wait for the migration lock.
    pub lock_wait: Duration,
    /// `openmig/<version> host=<host> user=<user>`.
    pub applied_by: String,
    /// Write history rows. False for scratch replays.
    pub record_history: bool,
}

impl Default for DeployOptions {
    fn default() -> Self {
        Self {
            to: None,
            baseline: None,
            allow_dirty: false,
            allow_out_of_order: false,
            dry_run: false,
            lock_timeout: Duration::from_secs(5),
            retries: 5,
            lock_wait: Duration::from_secs(600),
            applied_by: format!("openmig/{}", env!("CARGO_PKG_VERSION")),
            record_history: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    Applied {
        statements: u32,
        retries: u32,
    },
    /// A non-transactional file resumed after `skipped` already-applied statements.
    Resumed {
        skipped: u32,
        statements: u32,
    },
    Baselined,
    WouldApply {
        statements: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeployReport {
    pub files: Vec<(String, FileOutcome)>,
}

/// Callbacks while deploying (logging, progress output).
pub trait Progress: Send {
    fn file_started(&mut self, file: &MigrationFile, statements: usize);
    fn statement_done(&mut self, file: &MigrationFile, index: usize);
    fn file_done(&mut self, file: &MigrationFile, outcome: &FileOutcome);
    fn retrying(&mut self, file: &MigrationFile, attempt: u32, error: &str);
}

/// Apply pending files to the database behind `session`. See `docs/spec/formats.md`.
pub async fn deploy<E: Engine>(
    session: &mut E::Session,
    dir: &MigrationDir,
    options: &DeployOptions,
    progress: &mut dyn Progress,
) -> Result<DeployReport, Error> {
    let _ = (session, dir, options, progress);
    unimplemented!("deploy")
}

/// Apply every file to a scratch database with the same transaction semantics as `deploy`, but
/// without history or locking. Errors name the file and line of the failing statement.
pub async fn replay<E: Engine>(session: &mut E::Session, dir: &MigrationDir) -> Result<(), Error> {
    let _ = (session, dir);
    unimplemented!("replay")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusState {
    Applied,
    Baseline,
    Pending,
    /// Failed or interrupted; `deploy` will resume or retry it.
    Failed {
        error: Option<String>,
    },
    /// Applied, but the file changed afterwards.
    Modified,
    /// In history but the file no longer exists.
    Missing,
    /// Pending, but older than the newest applied version.
    OutOfOrder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    pub version: String,
    pub name: String,
    pub state: StatusState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub entries: Vec<StatusEntry>,
}

impl Status {
    pub fn pending(&self) -> impl Iterator<Item = &StatusEntry> {
        self.entries.iter().filter(|e| {
            matches!(
                e.state,
                StatusState::Pending | StatusState::OutOfOrder | StatusState::Failed { .. }
            )
        })
    }
}

/// Compare history with the directory.
pub fn status(history: &[HistoryRow], dir: &MigrationDir) -> Status {
    let _ = (history, dir);
    unimplemented!("status")
}
