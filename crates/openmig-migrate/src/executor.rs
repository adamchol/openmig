//! Applying migration files to a database.
//!
//! History rows relate to files as follows (see also `docs/spec/formats.md`):
//!
//! * An `applied` row (migration or baseline) whose file changed or no longer exists makes
//!   `deploy` refuse to run.
//! * A `failed`/`running` row whose file no longer exists is ignored with a warning when none of
//!   its statements were applied (a failed transactional file left nothing behind); when some were
//!   applied, `deploy` refuses, because the database holds effects no file accounts for.
//! * `--baseline` is honoured while the history holds nothing but baseline rows, so an interrupted
//!   baseline can be completed by rerunning the same command; once a migration has been applied
//!   it is ignored.

use crate::dir::{MigrationDir, MigrationFile};
use crate::sum::sha256_hex;
use jiff::Timestamp;
use openmig_core::sql::{self, SplitStatement};
use openmig_core::{
    Engine, Error, ExecError, ExecErrorKind, HistoryKind, HistoryRow, HistoryStatus, Scope, Session,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// The longest wait between two attempts of a file that hit a lock timeout.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct DeployOptions {
    /// Stop after this version (inclusive). Must be the version of a migration file.
    pub to: Option<String>,
    /// When history is empty: record every file up to and including this version as a baseline
    /// without running it.
    pub baseline: Option<String>,
    /// Allow a first deploy into a database that already has objects in scope.
    pub allow_dirty: bool,
    pub allow_out_of_order: bool,
    /// Report what would run without running it. Takes no lock and writes nothing; history is
    /// read when the table exists.
    pub dry_run: bool,
    /// Per-file lock timeout for transactional files (a file's `lock-timeout` directive wins).
    pub lock_timeout: Duration,
    /// How many times a transactional file is retried after a lock timeout.
    pub retries: u32,
    /// The wait before retry `n` (0-based) is `retry_backoff * 2^n`, at most 30 seconds.
    pub retry_backoff: Duration,
    /// How long to wait for the migration lock.
    pub lock_wait: Duration,
    /// `openmig/<version> host=<host> user=<user>`.
    pub applied_by: String,
    /// Read and write history rows. When false the history table is neither created, read nor
    /// written, so every file is pending.
    pub record_history: bool,
    /// What counts as "objects in scope" for the empty-database check of a first deploy.
    pub scope: Scope,
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
            retry_backoff: Duration::from_secs(1),
            lock_wait: Duration::from_secs(600),
            applied_by: format!("openmig/{}", env!("CARGO_PKG_VERSION")),
            record_history: true,
            scope: Scope::default(),
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
    /// Dry run: `statements` would run (excluding the applied prefix of a file being resumed).
    WouldApply {
        statements: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeployReport {
    /// `(file name, outcome)` in version order.
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
    if options.dry_run {
        return run::<E>(session, dir, options, progress).await;
    }
    session.lock(options.lock_wait).await?;
    let result = run::<E>(session, dir, options, progress).await;
    match (result, session.unlock().await) {
        (result, Ok(())) => result,
        (Ok(_), Err(unlock)) => Err(unlock),
        (Err(error), Err(unlock)) => {
            tracing::warn!(error = %unlock, "cannot release the migration lock");
            Err(error)
        }
    }
}

/// A pending file, split and checked before anything is applied.
struct PendingFile<'a> {
    file: &'a MigrationFile,
    statements: Vec<SplitStatement>,
    checksums: Vec<String>,
    transactional: bool,
    /// Statements already applied by an interrupted non-transactional run.
    resume_from: usize,
}

impl PendingFile<'_> {
    fn total(&self) -> u32 {
        count(self.statements.len())
    }

    fn row(
        &self,
        options: &DeployOptions,
        status: HistoryStatus,
        applied: usize,
        started_at: Timestamp,
    ) -> HistoryRow {
        HistoryRow {
            version: self.file.version.clone(),
            name: self.file.name.clone(),
            checksum: self.file.checksum.clone(),
            kind: HistoryKind::Migration,
            transactional: self.transactional,
            statements_total: self.total(),
            statements_applied: count(applied),
            statement_checksums: self.checksums[..applied].to_vec(),
            status,
            error: None,
            error_statement: None,
            started_at,
            finished_at: (status != HistoryStatus::Running).then(Timestamp::now),
            applied_by: options.applied_by.clone(),
        }
    }

    /// `error` located at statement `index` of this file (or at the file for setup and commit
    /// failures), keeping the server's details.
    fn locate(&self, error: ExecError, index: Option<usize>, retries: u32) -> ExecError {
        locate(self.file, &self.statements, error, index, retries)
    }
}

fn locate(
    file: &MigrationFile,
    statements: &[SplitStatement],
    error: ExecError,
    index: Option<usize>,
    retries: u32,
) -> ExecError {
    let after = match retries {
        0 => String::new(),
        1 => " after 1 retry".to_owned(),
        n => format!(" after {n} retries"),
    };
    let path = file.path.display();
    match index.and_then(|i| Some((i, statements.get(i)?))) {
        Some((i, statement)) => ExecError {
            message: format!(
                "{path}:{}: statement {} of {} failed{after}: {}",
                statement.line,
                i + 1,
                statements.len(),
                error.message
            ),
            statement: error.statement.or_else(|| Some(statement.sql.clone())),
            ..error
        },
        None => ExecError {
            message: format!("{path}: {}{after}", error.message),
            ..error
        },
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn split_file<E: Engine>(file: &MigrationFile) -> Result<Vec<SplitStatement>, Error> {
    sql::split(&file.content, E::DIALECT).map_err(|e| {
        Error::Migrations(format!("{}:{}: {}", file.path.display(), e.line, e.message))
    })
}

fn file_list<'a>(files: impl IntoIterator<Item = &'a str>) -> String {
    files.into_iter().collect::<Vec<_>>().join(", ")
}

fn backoff(base: Duration, retry: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(retry))
        .min(MAX_BACKOFF)
}

async fn run<E: Engine>(
    session: &mut E::Session,
    dir: &MigrationDir,
    options: &DeployOptions,
    progress: &mut dyn Progress,
) -> Result<DeployReport, Error> {
    let history = if options.record_history {
        if !options.dry_run {
            session.history_ensure().await?;
        }
        session.history_load().await?
    } else {
        Vec::new()
    };
    dir.verify_sum()
        .map_err(|e| Error::Migrations(e.to_string()))?;

    let rows: BTreeMap<&str, &HistoryRow> =
        history.iter().map(|r| (r.version.as_str(), r)).collect();
    check_history(&rows, dir)?;
    let baseline = baseline_files(&rows, dir, options)?;
    let pending = pending_files::<E>(session, &rows, &baseline, dir, options)?;

    if history.is_empty()
        && options.baseline.is_none()
        && !options.allow_dirty
        && !pending.is_empty()
    {
        let model = session.inspect(&options.scope).await?;
        if !E::is_empty(&model) {
            return Err(Error::Migrations(
                "the database already contains objects but has no migration history; \
                 use --baseline <version> to record the migrations it already contains as applied, \
                 or --allow-dirty to apply every migration anyway"
                    .to_owned(),
            ));
        }
    }

    let mut report = DeployReport::default();
    write_baselines::<E>(session, &baseline, options).await?;
    for file in &baseline {
        progress.file_done(file, &FileOutcome::Baselined);
        report
            .files
            .push((file.file_name.clone(), FileOutcome::Baselined));
    }

    for (index, pending) in pending.iter().enumerate() {
        let outcome = if options.dry_run {
            FileOutcome::WouldApply {
                statements: count(pending.statements.len() - pending.resume_from),
            }
        } else {
            if index > 0 {
                session.reset_session().await?;
            }
            progress.file_started(pending.file, pending.statements.len());
            if pending.transactional {
                apply_transactional::<E>(session, pending, options, progress).await?
            } else {
                apply_autocommit::<E>(session, pending, options, progress).await?
            }
        };
        progress.file_done(pending.file, &outcome);
        report.files.push((pending.file.file_name.clone(), outcome));
    }
    Ok(report)
}

/// Refuse when applied files changed or disappeared.
fn check_history(rows: &BTreeMap<&str, &HistoryRow>, dir: &MigrationDir) -> Result<(), Error> {
    let files: BTreeMap<&str, &MigrationFile> =
        dir.files.iter().map(|f| (f.version.as_str(), f)).collect();
    let mut changed = Vec::new();
    let mut missing = Vec::new();
    for (version, row) in rows {
        match (files.get(version), row.status) {
            (Some(file), HistoryStatus::Applied) if file.checksum != row.checksum => {
                changed.push(file.file_name.as_str());
            }
            (Some(_), _) => {}
            (None, HistoryStatus::Applied) => missing.push(*version),
            (None, _) if row.statements_applied > 0 => missing.push(*version),
            (None, status) => tracing::warn!(
                version,
                status = status.as_str(),
                "ignoring a history row whose migration file no longer exists"
            ),
        }
    }
    if !changed.is_empty() {
        return Err(Error::Migrations(format!(
            "migration files were changed after they were applied: {}; \
             restore them and put new changes in a new migration",
            file_list(changed)
        )));
    }
    if !missing.is_empty() {
        return Err(Error::Migrations(format!(
            "the database history records migrations that are not in {}: {}; \
             restore the files, or deploy the directory this database was migrated with",
            dir.path.display(),
            file_list(missing)
        )));
    }
    Ok(())
}

fn baseline_files<'a>(
    rows: &BTreeMap<&str, &HistoryRow>,
    dir: &'a MigrationDir,
    options: &DeployOptions,
) -> Result<Vec<&'a MigrationFile>, Error> {
    let Some(target) = options.baseline.as_deref() else {
        return Ok(Vec::new());
    };
    if rows.values().any(|r| r.kind != HistoryKind::Baseline) {
        tracing::debug!(
            baseline = target,
            "history is not empty; ignoring --baseline"
        );
        return Ok(Vec::new());
    }
    if !dir.files.iter().any(|f| f.version == target) {
        return Err(Error::Migrations(format!(
            "--baseline {target} does not match any migration file in {}",
            dir.path.display()
        )));
    }
    Ok(dir
        .files
        .iter()
        .filter(|f| f.version.as_str() <= target && !rows.contains_key(f.version.as_str()))
        .collect())
}

async fn write_baselines<E: Engine>(
    session: &mut E::Session,
    files: &[&MigrationFile],
    options: &DeployOptions,
) -> Result<(), Error> {
    if files.is_empty() || options.dry_run || !options.record_history {
        return Ok(());
    }
    let now = Timestamp::now();
    let rows = files.iter().map(|file| HistoryRow {
        version: file.version.clone(),
        name: file.name.clone(),
        checksum: file.checksum.clone(),
        kind: HistoryKind::Baseline,
        transactional: false,
        statements_total: 0,
        statements_applied: 0,
        statement_checksums: Vec::new(),
        status: HistoryStatus::Applied,
        error: None,
        error_statement: None,
        started_at: now,
        finished_at: Some(now),
        applied_by: options.applied_by.clone(),
    });
    let transactional = session.capabilities().transactional_ddl;
    if transactional {
        session.begin().await?;
    }
    for row in rows {
        if let Err(error) = session.history_write(&row).await {
            if transactional {
                rollback_quietly::<E>(session).await;
            }
            return Err(error);
        }
    }
    if transactional {
        session.commit().await?;
    }
    Ok(())
}

/// Files to apply in this run, in version order, split and checked.
fn pending_files<'a, E: Engine>(
    session: &E::Session,
    rows: &BTreeMap<&str, &HistoryRow>,
    baseline: &[&MigrationFile],
    dir: &'a MigrationDir,
    options: &DeployOptions,
) -> Result<Vec<PendingFile<'a>>, Error> {
    if let Some(to) = options.to.as_deref()
        && !dir.files.iter().any(|f| f.version == to)
    {
        return Err(Error::Migrations(format!(
            "--to {to} does not match any migration file in {}",
            dir.path.display()
        )));
    }
    let baselined: BTreeSet<&str> = baseline.iter().map(|f| f.version.as_str()).collect();
    let applied = |version: &str| {
        baselined.contains(version)
            || rows
                .get(version)
                .is_some_and(|r| r.status == HistoryStatus::Applied)
    };
    let newest = rows
        .values()
        .filter(|r| r.status == HistoryStatus::Applied)
        .map(|r| r.version.as_str())
        .chain(baselined.iter().copied())
        .max();
    let files: Vec<&MigrationFile> = dir
        .files
        .iter()
        .filter(|f| !applied(&f.version))
        .filter(|f| {
            options
                .to
                .as_deref()
                .is_none_or(|to| f.version.as_str() <= to)
        })
        .collect();

    if let Some(newest) = newest
        && !options.allow_out_of_order
    {
        let older: Vec<&str> = files
            .iter()
            .filter(|f| f.version.as_str() < newest)
            .map(|f| f.file_name.as_str())
            .collect();
        if !older.is_empty() {
            return Err(Error::Migrations(format!(
                "pending migrations are older than the newest applied version {newest}: {}; \
                 give them newer versions (regenerate or rename them), or use --allow-out-of-order",
                file_list(older)
            )));
        }
    }

    let transactional_ddl = session.capabilities().transactional_ddl;
    files
        .into_iter()
        .map(|file| {
            let statements = split_file::<E>(file)?;
            let checksums: Vec<String> = statements
                .iter()
                .map(|s| sha256_hex(s.sql.as_bytes()))
                .collect();
            let transactional = transactional_ddl && !file.directives.no_transaction;
            let row = rows.get(file.version.as_str());
            let resume_from = row.map_or(0, |r| r.statements_applied as usize);
            if resume_from > 0 {
                let path = file.path.display();
                if transactional {
                    return Err(Error::Migrations(format!(
                        "{path}: {resume_from} statements were applied without a transaction by an \
                         earlier deploy; restore `-- openmig:no-transaction` so the file can resume"
                    )));
                }
                let recorded = row.map_or(&[][..], |r| &r.statement_checksums[..]);
                if resume_from > checksums.len()
                    || recorded.len() < resume_from
                    || recorded[..resume_from] != checksums[..resume_from]
                {
                    return Err(Error::Migrations(format!(
                        "{path}: the file changed after {resume_from} of its statements were \
                         applied; restore those statements (only the failed statement and the ones \
                         after it may change)"
                    )));
                }
            }
            Ok(PendingFile {
                file,
                statements,
                checksums,
                transactional,
                resume_from,
            })
        })
        .collect()
}

/// Why applying a file stopped: a statement (or setup/commit when `index` is `None`) failed on
/// the server, or the history could not be written.
enum Failure {
    Exec {
        index: Option<usize>,
        error: ExecError,
    },
    History(Error),
}

async fn apply_transactional<E: Engine>(
    session: &mut E::Session,
    pending: &PendingFile<'_>,
    options: &DeployOptions,
    progress: &mut dyn Progress,
) -> Result<FileOutcome, Error> {
    let mut retries = 0;
    loop {
        let started_at = Timestamp::now();
        session
            .begin()
            .await
            .map_err(|e| pending.locate(e, None, retries))?;
        let result =
            match transaction_body::<E>(session, pending, options, progress, started_at).await {
                Ok(()) => session
                    .commit()
                    .await
                    .map_err(|error| Failure::Exec { index: None, error }),
                Err(failure) => {
                    rollback_quietly::<E>(session).await;
                    Err(failure)
                }
            };
        let (index, error) = match result {
            Ok(()) => {
                return Ok(FileOutcome::Applied {
                    statements: pending.total(),
                    retries,
                });
            }
            Err(Failure::History(error)) => return Err(error),
            Err(Failure::Exec { index, error }) => (index, error),
        };
        if error.kind == ExecErrorKind::LockTimeout && retries < options.retries {
            progress.retrying(pending.file, retries + 1, &error.to_string());
            tokio::time::sleep(backoff(options.retry_backoff, retries)).await;
            retries += 1;
            continue;
        }
        if options.record_history {
            let mut row = pending.row(options, HistoryStatus::Failed, 0, started_at);
            row.error = Some(error.to_string());
            row.error_statement = index.map(count);
            write_failure_row::<E>(session, &row).await;
        }
        return Err(Error::Exec(pending.locate(error, index, retries)));
    }
}

async fn transaction_body<E: Engine>(
    session: &mut E::Session,
    pending: &PendingFile<'_>,
    options: &DeployOptions,
    progress: &mut dyn Progress,
    started_at: Timestamp,
) -> Result<(), Failure> {
    let directives = &pending.file.directives;
    let settings = [
        E::lock_timeout_sql(directives.lock_timeout.unwrap_or(options.lock_timeout)),
        directives
            .statement_timeout
            .and_then(E::statement_timeout_sql),
    ];
    for sql in settings.iter().flatten() {
        session
            .execute(sql)
            .await
            .map_err(|error| Failure::Exec { index: None, error })?;
    }
    for (index, statement) in pending.statements.iter().enumerate() {
        session
            .execute(&statement.sql)
            .await
            .map_err(|error| Failure::Exec {
                index: Some(index),
                error,
            })?;
        progress.statement_done(pending.file, index);
    }
    if options.record_history {
        let row = pending.row(
            options,
            HistoryStatus::Applied,
            pending.statements.len(),
            started_at,
        );
        session
            .history_write(&row)
            .await
            .map_err(Failure::History)?;
    }
    Ok(())
}

async fn apply_autocommit<E: Engine>(
    session: &mut E::Session,
    pending: &PendingFile<'_>,
    options: &DeployOptions,
    progress: &mut dyn Progress,
) -> Result<FileOutcome, Error> {
    let start = pending.resume_from;
    let mut row = pending.row(options, HistoryStatus::Running, start, Timestamp::now());
    if options.record_history {
        session.history_write(&row).await?;
    }
    let result = autocommit_body::<E>(session, pending, &mut row, options, progress).await;
    row.finished_at = Some(Timestamp::now());
    match result {
        Ok(()) => {
            row.status = HistoryStatus::Applied;
            if options.record_history {
                session.history_write(&row).await?;
            }
            let remaining = pending.total() - count(start);
            Ok(if start > 0 {
                FileOutcome::Resumed {
                    skipped: count(start),
                    statements: remaining,
                }
            } else {
                FileOutcome::Applied {
                    statements: remaining,
                    retries: 0,
                }
            })
        }
        Err(Failure::History(error)) => Err(error),
        Err(Failure::Exec { index, error }) => {
            if options.record_history {
                row.status = HistoryStatus::Failed;
                row.error = Some(error.to_string());
                row.error_statement = index.map(count);
                write_failure_row::<E>(session, &row).await;
            }
            Err(Error::Exec(pending.locate(error, index, 0)))
        }
    }
}

/// Run the statements after the applied prefix, recording progress in `row` after each one.
async fn autocommit_body<E: Engine>(
    session: &mut E::Session,
    pending: &PendingFile<'_>,
    row: &mut HistoryRow,
    options: &DeployOptions,
    progress: &mut dyn Progress,
) -> Result<(), Failure> {
    if let Some(sql) = pending
        .file
        .directives
        .statement_timeout
        .and_then(E::statement_timeout_sql)
    {
        session
            .execute(&sql)
            .await
            .map_err(|error| Failure::Exec { index: None, error })?;
    }
    for index in pending.resume_from..pending.statements.len() {
        session
            .execute(&pending.statements[index].sql)
            .await
            .map_err(|error| Failure::Exec {
                index: Some(index),
                error,
            })?;
        row.statements_applied = count(index + 1);
        row.statement_checksums
            .push(pending.checksums[index].clone());
        if options.record_history {
            session.history_write(row).await.map_err(Failure::History)?;
        }
        progress.statement_done(pending.file, index);
    }
    Ok(())
}

/// Record a failure; the statement error matters more than a failure to record it.
async fn write_failure_row<E: Engine>(session: &mut E::Session, row: &HistoryRow) {
    if let Err(error) = session.history_write(row).await {
        tracing::warn!(version = row.version, %error, "cannot record the failed migration");
    }
}

async fn rollback_quietly<E: Engine>(session: &mut E::Session) {
    if let Err(error) = session.rollback().await {
        tracing::warn!(%error, "rollback failed");
    }
}

/// Apply every file to a scratch database with the same transaction semantics as `deploy`, but
/// without history or locking. Errors name the file and line of the failing statement.
pub async fn replay<E: Engine>(session: &mut E::Session, dir: &MigrationDir) -> Result<(), Error> {
    let files = dir
        .files
        .iter()
        .map(|file| Ok((file, split_file::<E>(file)?)))
        .collect::<Result<Vec<_>, Error>>()?;
    let transactional_ddl = session.capabilities().transactional_ddl;
    for (n, (file, statements)) in files.iter().enumerate() {
        let fail = |error: ExecError, index: Option<usize>| {
            Error::Exec(locate(file, statements, error, index, 0))
        };
        if n > 0 {
            session.reset_session().await.map_err(|e| fail(e, None))?;
        }
        let transactional = transactional_ddl && !file.directives.no_transaction;
        if transactional {
            session.begin().await.map_err(|e| fail(e, None))?;
        }
        for (index, statement) in statements.iter().enumerate() {
            if let Err(error) = session.execute(&statement.sql).await {
                if transactional {
                    rollback_quietly::<E>(session).await;
                }
                return Err(fail(error, Some(index)));
            }
        }
        if transactional {
            session.commit().await.map_err(|e| fail(e, None))?;
        }
    }
    Ok(())
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

/// Compare history with the directory: one entry per version found in either, in version order.
/// Baseline rows count as applied; a changed file is `Modified` whether it was applied or
/// baselined; a history row without a file is `Missing` whatever its status.
pub fn status(history: &[HistoryRow], dir: &MigrationDir) -> Status {
    let newest_applied = history
        .iter()
        .filter(|r| r.status == HistoryStatus::Applied)
        .map(|r| r.version.as_str())
        .max();
    let mut versions: BTreeMap<&str, (Option<&MigrationFile>, Option<&HistoryRow>)> =
        BTreeMap::new();
    for file in &dir.files {
        versions.entry(&file.version).or_default().0 = Some(file);
    }
    for row in history {
        versions.entry(&row.version).or_default().1 = Some(row);
    }
    let entries = versions
        .into_iter()
        .map(|(version, (file, row))| {
            let state = match (file, row) {
                (Some(file), Some(row)) => match row.status {
                    HistoryStatus::Applied if row.checksum != file.checksum => {
                        StatusState::Modified
                    }
                    HistoryStatus::Applied if row.kind == HistoryKind::Baseline => {
                        StatusState::Baseline
                    }
                    HistoryStatus::Applied => StatusState::Applied,
                    HistoryStatus::Failed | HistoryStatus::Running => StatusState::Failed {
                        error: row.error.clone(),
                    },
                },
                (Some(_), None) if newest_applied.is_some_and(|newest| version < newest) => {
                    StatusState::OutOfOrder
                }
                (Some(_), None) => StatusState::Pending,
                (None, _) => StatusState::Missing,
            };
            let name = file
                .map(|f| f.name.clone())
                .or_else(|| row.map(|r| r.name.clone()))
                .unwrap_or_default();
            StatusEntry {
                version: version.to_owned(),
                name,
                state,
            }
        })
        .collect();
    Status { entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_a_cap() {
        let base = Duration::from_millis(500);
        let waits: Vec<_> = (0..8).map(|n| backoff(base, n).as_millis()).collect();
        assert_eq!(waits, [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000]);
        assert_eq!(backoff(base, 100), MAX_BACKOFF);
        assert_eq!(backoff(Duration::ZERO, 3), Duration::ZERO);
    }
}
