//! An in-memory engine for executor tests: records every call, keeps history in memory, models
//! transactions and fails statements on request.

use openmig_core::sql::Dialect;
use openmig_core::{
    Capabilities, ChangeInfo, Desired, DiffContext, EncodeOptions, EncodedFile, Engine, Error,
    ExecError, ExecErrorKind, HistoryRow, ObjectRef, Plan, RenameCandidate, RenameHints, RenameSet,
    Scope, ServerVersion, Session, SessionPurpose,
};
use openmig_migrate::{FileOutcome, MigrationFile, Progress};
use std::collections::BTreeMap;
use std::time::Duration;

pub struct MockEngine;

#[derive(Clone, Debug)]
pub struct MockChange;

impl ChangeInfo for MockChange {
    fn object(&self) -> ObjectRef {
        unimplemented!()
    }
    fn summary(&self) -> String {
        unimplemented!()
    }
    fn destructive(&self) -> Option<String> {
        unimplemented!()
    }
}

impl Engine for MockEngine {
    const NAME: &'static str = "mock";
    const DIALECT: Dialect = Dialect::Postgres;
    type Settings = ();
    type Model = Vec<String>;
    type Change = MockChange;
    type Session = MockSession;

    fn schema_spec(_: &()) -> openmig_hcl::SchemaSpec {
        unimplemented!()
    }
    fn decode(_: &openmig_hcl::Document, _: &()) -> Result<Desired<Vec<String>>, Error> {
        unimplemented!()
    }
    fn encode(_: &Vec<String>, _: &(), _: &EncodeOptions) -> Vec<EncodedFile> {
        unimplemented!()
    }
    fn restrict(_: &mut Vec<String>, _: &Scope) {}
    fn declared_roles(_: &Vec<String>) -> Vec<String> {
        Vec::new()
    }
    fn data_tables(_: &Vec<String>) -> Vec<openmig_core::DataTable> {
        Vec::new()
    }
    fn is_empty(model: &Vec<String>) -> bool {
        model.is_empty()
    }
    fn resolve_hints(_: &Vec<String>, _: &Vec<String>, _: &RenameHints) -> RenameSet {
        unimplemented!()
    }
    fn rename_candidates(_: &Vec<String>, _: &Vec<String>) -> Vec<RenameCandidate> {
        unimplemented!()
    }
    fn diff(
        _: &Vec<String>,
        _: &Vec<String>,
        _: &DiffContext<'_, Self>,
    ) -> Result<Vec<MockChange>, Error> {
        unimplemented!()
    }
    fn plan(
        _: Vec<MockChange>,
        _: &Vec<String>,
        _: &Vec<String>,
        _: &DiffContext<'_, Self>,
    ) -> Result<Plan, Error> {
        unimplemented!()
    }
    fn lock_timeout_sql(timeout: Duration) -> Option<String> {
        Some(format!(
            "SET LOCAL lock_timeout = '{}ms'",
            timeout.as_millis()
        ))
    }
    fn statement_timeout_sql(timeout: Duration) -> Option<String> {
        Some(format!(
            "SET statement_timeout = '{}ms'",
            timeout.as_millis()
        ))
    }
    fn scratch_spec(_: &(), _: Option<u32>) -> openmig_scratch::Spec {
        unimplemented!()
    }
    async fn connect(_: &str, _: &(), _: SessionPurpose) -> Result<MockSession, Error> {
        unimplemented!()
    }
}

enum Matcher {
    Contains(String),
    Nth(usize),
}

struct Failure {
    matcher: Matcher,
    kind: ExecErrorKind,
    /// `None`: fail every time.
    remaining: Option<u32>,
}

pub struct MockSession {
    pub transactional: bool,
    version: ServerVersion,
    /// Every call in order: `LOCK`, `UNLOCK`, `ENSURE`, `LOAD`, `INSPECT`, `BEGIN`, `COMMIT`,
    /// `ROLLBACK`, `RESET`, `WRITE <version> <status> <applied>`, or the executed SQL.
    pub log: Vec<String>,
    /// Statements whose effects persist: rolled-back statements are removed.
    pub effects: Vec<String>,
    pub history: BTreeMap<String, HistoryRow>,
    /// Returned by `inspect`.
    pub objects: Vec<String>,
    /// Another deploy holds the lock.
    pub lock_taken: bool,
    pub locked: bool,
    failures: Vec<Failure>,
    executions: usize,
    tx: Option<(usize, BTreeMap<String, HistoryRow>)>,
}

impl MockSession {
    pub fn new(transactional: bool) -> Self {
        Self {
            transactional,
            version: ServerVersion {
                major: 17,
                minor: 0,
                text: "17.0".to_owned(),
            },
            log: Vec::new(),
            effects: Vec::new(),
            history: BTreeMap::new(),
            objects: Vec::new(),
            lock_taken: false,
            locked: false,
            failures: Vec::new(),
            executions: 0,
            tx: None,
        }
    }

    /// Fail statements containing `pattern`, `times` times (`None`: always).
    pub fn fail_on(&mut self, pattern: &str, kind: ExecErrorKind, times: Option<u32>) {
        self.failures.push(Failure {
            matcher: Matcher::Contains(pattern.to_owned()),
            kind,
            remaining: times,
        });
    }

    /// Fail the `n`th (0-based) call to `execute` of this session.
    pub fn fail_at(&mut self, n: usize, kind: ExecErrorKind) {
        self.failures.push(Failure {
            matcher: Matcher::Nth(n),
            kind,
            remaining: Some(1),
        });
    }

    pub fn clear_failures(&mut self) {
        self.failures.clear();
    }

    pub fn in_transaction(&self) -> bool {
        self.tx.is_some()
    }

    /// Effects without session settings.
    pub fn ddl(&self) -> Vec<&str> {
        self.effects
            .iter()
            .map(String::as_str)
            .filter(|s| !s.starts_with("SET "))
            .collect()
    }

    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }
}

impl Session<MockEngine> for MockSession {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactional_ddl: self.transactional,
            savepoints: self.transactional,
        }
    }

    fn server_version(&self) -> &ServerVersion {
        &self.version
    }

    async fn inspect(&mut self, _: &Scope) -> Result<Vec<String>, Error> {
        self.log.push("INSPECT".to_owned());
        Ok(self.objects.clone())
    }

    async fn execute(&mut self, sql: &str) -> Result<(), ExecError> {
        self.log.push(sql.to_owned());
        let n = self.executions;
        self.executions += 1;
        for failure in &mut self.failures {
            let matches = match &failure.matcher {
                Matcher::Contains(pattern) => sql.contains(pattern.as_str()),
                Matcher::Nth(nth) => *nth == n,
            };
            if !matches || failure.remaining == Some(0) {
                continue;
            }
            if let Some(remaining) = &mut failure.remaining {
                *remaining -= 1;
            }
            let code = match failure.kind {
                ExecErrorKind::LockTimeout => "55P03",
                _ => "XX000",
            };
            return Err(ExecError {
                kind: failure.kind,
                message: format!("mock failure: {sql}"),
                code: Some(code.to_owned()),
                hint: Some("mock hint".to_owned()),
                ..ExecError::default()
            });
        }
        self.effects.push(sql.to_owned());
        Ok(())
    }

    async fn begin(&mut self) -> Result<(), ExecError> {
        self.log.push("BEGIN".to_owned());
        if self.tx.is_some() {
            return Err(ExecError::new("already in a transaction"));
        }
        self.tx = Some((self.effects.len(), self.history.clone()));
        Ok(())
    }

    async fn commit(&mut self) -> Result<(), ExecError> {
        self.log.push("COMMIT".to_owned());
        self.tx
            .take()
            .map(|_| ())
            .ok_or_else(|| ExecError::new("no transaction"))
    }

    async fn rollback(&mut self) -> Result<(), ExecError> {
        self.log.push("ROLLBACK".to_owned());
        if let Some((effects, history)) = self.tx.take() {
            self.effects.truncate(effects);
            self.history = history;
        }
        Ok(())
    }

    async fn lock(&mut self, _: Duration) -> Result<(), Error> {
        self.log.push("LOCK".to_owned());
        if self.lock_taken {
            return Err(Error::LockTimeout("held by another deploy".to_owned()));
        }
        self.locked = true;
        Ok(())
    }

    async fn unlock(&mut self) -> Result<(), Error> {
        self.log.push("UNLOCK".to_owned());
        self.locked = false;
        Ok(())
    }

    async fn history_ensure(&mut self) -> Result<(), Error> {
        self.log.push("ENSURE".to_owned());
        Ok(())
    }

    async fn history_load(&mut self) -> Result<Vec<HistoryRow>, Error> {
        self.log.push("LOAD".to_owned());
        Ok(self.history.values().cloned().collect())
    }

    async fn history_write(&mut self, row: &HistoryRow) -> Result<(), Error> {
        self.log.push(format!(
            "WRITE {} {} {}",
            row.version,
            row.status.as_str(),
            row.statements_applied
        ));
        self.history.insert(row.version.clone(), row.clone());
        Ok(())
    }

    async fn history_delete(&mut self, version: &str) -> Result<(), Error> {
        self.history.remove(version);
        Ok(())
    }

    async fn reset_session(&mut self) -> Result<(), ExecError> {
        self.log.push("RESET".to_owned());
        assert!(self.tx.is_none(), "RESET inside a transaction");
        Ok(())
    }

    async fn reset(&mut self) -> Result<(), Error> {
        unimplemented!()
    }
}

/// Records progress callbacks as strings.
#[derive(Default)]
pub struct Events(pub Vec<String>);

impl Progress for Events {
    fn file_started(&mut self, file: &MigrationFile, statements: usize) {
        self.0.push(format!("start {} {statements}", file.version));
    }
    fn statement_done(&mut self, file: &MigrationFile, index: usize) {
        self.0.push(format!("done {} {index}", file.version));
    }
    fn file_done(&mut self, file: &MigrationFile, outcome: &FileOutcome) {
        self.0.push(format!("finish {} {outcome:?}", file.version));
    }
    fn retrying(&mut self, file: &MigrationFile, attempt: u32, error: &str) {
        self.0
            .push(format!("retry {} {attempt}: {error}", file.version));
    }
}
