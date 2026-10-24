mod mock;

use mock::{Events, MockEngine, MockSession};
use openmig_core::{Error, ExecErrorKind, HistoryKind, HistoryRow, HistoryStatus};
use openmig_migrate::{
    DeployOptions, DeployReport, FileOutcome, MigrationDir, StatusState, sha256_hex,
};
use pretty_assertions::assert_eq;
use std::time::Duration;

const A: (&str, &str) = (
    "20260101000000_a.sql",
    "-- Create t1\nCREATE TABLE t1 (id int);\n-- Create t2\nCREATE TABLE t2 (id int);\n",
);
const B: (&str, &str) = (
    "20260102000000_b.sql",
    "CREATE TABLE t3 (id int);\nCREATE INDEX i3 ON t3 (id);\n",
);
const C: (&str, &str) = (
    "20260103000000_c.sql",
    "-- openmig:no-transaction\n\nCREATE INDEX CONCURRENTLY i1 ON t1 (id);\nCREATE INDEX CONCURRENTLY i2 ON t2 (id);\nCREATE INDEX CONCURRENTLY i4 ON t4 (id);\n",
);
const VA: &str = "20260101000000";
const VB: &str = "20260102000000";
const VC: &str = "20260103000000";

struct Migrations {
    tmp: tempfile::TempDir,
    dir: MigrationDir,
}

impl Migrations {
    fn new(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(tmp.path().join(name), content).unwrap();
        }
        let dir = MigrationDir::load(tmp.path()).unwrap();
        dir.write_sum().unwrap();
        Self { tmp, dir }
    }

    /// Write a file and rehash.
    fn write(&mut self, name: &str, content: &str) {
        std::fs::write(self.tmp.path().join(name), content).unwrap();
        self.dir = MigrationDir::load(self.tmp.path()).unwrap();
        self.dir.write_sum().unwrap();
    }

    fn file(&self, version: &str) -> &openmig_migrate::MigrationFile {
        self.dir
            .files
            .iter()
            .find(|f| f.version == version)
            .unwrap()
    }
}

fn options() -> DeployOptions {
    DeployOptions {
        retry_backoff: Duration::ZERO,
        applied_by: "openmig/test".to_owned(),
        ..DeployOptions::default()
    }
}

async fn deploy(
    session: &mut MockSession,
    dir: &MigrationDir,
    options: &DeployOptions,
) -> (Result<DeployReport, Error>, Vec<String>) {
    let mut events = Events::default();
    let result = openmig_migrate::deploy::<MockEngine>(session, dir, options, &mut events).await;
    assert!(!session.locked, "the lock was not released");
    assert!(!session.in_transaction(), "a transaction was left open");
    (result, events.0)
}

fn report(files: &[(&str, FileOutcome)]) -> DeployReport {
    DeployReport {
        files: files
            .iter()
            .map(|(name, outcome)| (name.to_string(), outcome.clone()))
            .collect(),
    }
}

fn applied(statements: u32) -> FileOutcome {
    FileOutcome::Applied {
        statements,
        retries: 0,
    }
}

fn migrations_error(result: Result<DeployReport, Error>) -> String {
    match result {
        Err(Error::Migrations(message)) => message,
        other => panic!("expected a migrations error, got {other:?}"),
    }
}

fn checksums(statements: &[&str]) -> Vec<String> {
    statements
        .iter()
        .map(|s| sha256_hex(s.as_bytes()))
        .collect()
}

fn row(version: &str, kind: HistoryKind, status: HistoryStatus, checksum: &str) -> HistoryRow {
    HistoryRow {
        version: version.to_owned(),
        name: String::new(),
        checksum: checksum.to_owned(),
        kind,
        transactional: true,
        statements_total: 0,
        statements_applied: 0,
        statement_checksums: Vec::new(),
        status,
        error: None,
        error_statement: None,
        started_at: jiff::Timestamp::UNIX_EPOCH,
        finished_at: None,
        applied_by: "test".to_owned(),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn fresh_deploy() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(
        result.unwrap(),
        report(&[(A.0, applied(2)), (B.0, applied(2)), (C.0, applied(3))])
    );
    assert_eq!(
        session.log,
        strings(&[
            "LOCK",
            "ENSURE",
            "LOAD",
            "INSPECT",
            "BEGIN",
            "SET LOCAL lock_timeout = '5000ms'",
            "CREATE TABLE t1 (id int)",
            "CREATE TABLE t2 (id int)",
            "WRITE 20260101000000 applied 2",
            "COMMIT",
            "RESET",
            "BEGIN",
            "SET LOCAL lock_timeout = '5000ms'",
            "CREATE TABLE t3 (id int)",
            "CREATE INDEX i3 ON t3 (id)",
            "WRITE 20260102000000 applied 2",
            "COMMIT",
            "RESET",
            "WRITE 20260103000000 running 0",
            "CREATE INDEX CONCURRENTLY i1 ON t1 (id)",
            "WRITE 20260103000000 running 1",
            "CREATE INDEX CONCURRENTLY i2 ON t2 (id)",
            "WRITE 20260103000000 running 2",
            "CREATE INDEX CONCURRENTLY i4 ON t4 (id)",
            "WRITE 20260103000000 running 3",
            "WRITE 20260103000000 applied 3",
            "UNLOCK",
        ])
    );
    assert_eq!(
        events,
        strings(&[
            "start 20260101000000 2",
            "done 20260101000000 0",
            "done 20260101000000 1",
            "finish 20260101000000 Applied { statements: 2, retries: 0 }",
            "start 20260102000000 2",
            "done 20260102000000 0",
            "done 20260102000000 1",
            "finish 20260102000000 Applied { statements: 2, retries: 0 }",
            "start 20260103000000 3",
            "done 20260103000000 0",
            "done 20260103000000 1",
            "done 20260103000000 2",
            "finish 20260103000000 Applied { statements: 3, retries: 0 }",
        ])
    );

    let a = &session.history[VA];
    assert_eq!(a.name, "a");
    assert_eq!(a.checksum, m.file(VA).checksum);
    assert_eq!(a.kind, HistoryKind::Migration);
    assert_eq!(a.status, HistoryStatus::Applied);
    assert!(a.transactional);
    assert_eq!((a.statements_total, a.statements_applied), (2, 2));
    assert_eq!(
        a.statement_checksums,
        checksums(&["CREATE TABLE t1 (id int)", "CREATE TABLE t2 (id int)"])
    );
    assert_eq!(a.applied_by, "openmig/test");
    assert!(a.finished_at.is_some());
    let c = &session.history[VC];
    assert!(!c.transactional);
    assert_eq!(c.status, HistoryStatus::Applied);
    assert_eq!((c.statements_total, c.statements_applied), (3, 3));
    assert_eq!(c.statement_checksums.len(), 3);
    assert_eq!((c.error.as_deref(), c.error_statement), (None, None));
}

#[tokio::test]
async fn nothing_pending_is_idempotent() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    deploy(&mut session, &m.dir, &options()).await.0.unwrap();
    let effects = session.effects.clone();
    let history = session.history.clone();
    session.take_log();
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(result.unwrap(), DeployReport::default());
    assert!(events.is_empty());
    assert_eq!(session.log, strings(&["LOCK", "ENSURE", "LOAD", "UNLOCK"]));
    assert_eq!(session.effects, effects);
    assert_eq!(session.history, history);
}

#[tokio::test]
async fn empty_directory_does_nothing() {
    let m = Migrations::new(&[]);
    let mut session = MockSession::new(true);
    session.objects = strings(&["table app.users"]);
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(result.unwrap(), DeployReport::default());
    assert_eq!(session.log, strings(&["LOCK", "ENSURE", "LOAD", "UNLOCK"]));
}

#[tokio::test]
async fn baseline() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    session.objects = strings(&["table t1"]);
    let opts = DeployOptions {
        baseline: Some(VB.to_owned()),
        ..options()
    };
    let (result, events) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(
        result.unwrap(),
        report(&[
            (A.0, FileOutcome::Baselined),
            (B.0, FileOutcome::Baselined),
            (C.0, applied(3))
        ])
    );
    assert_eq!(
        &events[..2],
        [
            "finish 20260101000000 Baselined",
            "finish 20260102000000 Baselined"
        ]
    );
    assert_eq!(
        &session.log[..8],
        strings(&[
            "LOCK",
            "ENSURE",
            "LOAD",
            "BEGIN",
            "WRITE 20260101000000 applied 0",
            "WRITE 20260102000000 applied 0",
            "COMMIT",
            "WRITE 20260103000000 running 0",
        ])
    );
    assert!(!session.log.contains(&"INSPECT".to_owned()));
    assert_eq!(
        session.ddl(),
        [
            "CREATE INDEX CONCURRENTLY i1 ON t1 (id)",
            "CREATE INDEX CONCURRENTLY i2 ON t2 (id)",
            "CREATE INDEX CONCURRENTLY i4 ON t4 (id)"
        ]
    );
    for version in [VA, VB] {
        let row = &session.history[version];
        assert_eq!(row.kind, HistoryKind::Baseline);
        assert_eq!(row.status, HistoryStatus::Applied);
        assert_eq!((row.statements_total, row.statements_applied), (0, 0));
        assert_eq!(row.checksum, m.file(version).checksum);
    }
    assert_eq!(session.history[VC].kind, HistoryKind::Migration);

    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(
        result.unwrap(),
        DeployReport::default(),
        "--baseline is harmless on reruns"
    );
}

#[tokio::test]
async fn baseline_must_name_a_file() {
    let m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        baseline: Some("20250101000000".to_owned()),
        ..options()
    };
    let message = migrations_error(deploy(&mut session, &m.dir, &opts).await.0);
    assert!(
        message.contains("--baseline 20250101000000 does not match any migration file"),
        "{message}"
    );
    assert!(session.history.is_empty());
}

#[tokio::test]
async fn baseline_completes_an_interrupted_baseline_but_not_after_migrations() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    session.history.insert(
        VA.to_owned(),
        row(
            VA,
            HistoryKind::Baseline,
            HistoryStatus::Applied,
            &m.file(VA).checksum,
        ),
    );
    let opts = DeployOptions {
        baseline: Some(VB.to_owned()),
        to: Some(VB.to_owned()),
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(result.unwrap(), report(&[(B.0, FileOutcome::Baselined)]));

    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    let to_a = DeployOptions {
        to: Some(VA.to_owned()),
        ..options()
    };
    deploy(&mut session, &m.dir, &to_a).await.0.unwrap();
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(result.unwrap(), report(&[(B.0, applied(2))]));
}

#[tokio::test]
async fn dirty_database_is_refused() {
    let m = Migrations::new(&[A]);
    let mut session = MockSession::new(true);
    session.objects = strings(&["table app.users"]);
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(message.contains("--baseline"), "{message}");
    assert!(message.contains("--allow-dirty"), "{message}");
    assert_eq!(
        session.log,
        strings(&["LOCK", "ENSURE", "LOAD", "INSPECT", "UNLOCK"])
    );
    assert!(session.history.is_empty());
}

#[tokio::test]
async fn allow_dirty() {
    let m = Migrations::new(&[A]);
    let mut session = MockSession::new(true);
    session.objects = strings(&["table app.users"]);
    let opts = DeployOptions {
        allow_dirty: true,
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(result.unwrap(), report(&[(A.0, applied(2))]));
    assert!(!session.log.contains(&"INSPECT".to_owned()));
}

#[tokio::test]
async fn changed_applied_file_is_refused() {
    let mut m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(true);
    deploy(&mut session, &m.dir, &options()).await.0.unwrap();
    m.write(A.0, "CREATE TABLE t1 (id bigint);\n");
    m.write("20260104000000_d.sql", "SELECT 1;\n");
    let effects = session.effects.clone();
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains("changed after they were applied: 20260101000000_a.sql"),
        "{message}"
    );
    assert_eq!(session.effects, effects);
}

#[tokio::test]
async fn changed_baselined_file_is_refused() {
    let mut m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        baseline: Some(VA.to_owned()),
        ..options()
    };
    deploy(&mut session, &m.dir, &opts).await.0.unwrap();
    m.write(A.0, "CREATE TABLE t1 (id bigint);\n");
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(message.contains("20260101000000_a.sql"), "{message}");
}

#[tokio::test]
async fn missing_applied_file_is_refused() {
    let m = Migrations::new(&[B]);
    let mut session = MockSession::new(true);
    session.history.insert(
        VA.to_owned(),
        row(VA, HistoryKind::Migration, HistoryStatus::Applied, "x"),
    );
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains("records migrations that are not in") && message.contains(VA),
        "{message}"
    );
}

#[tokio::test]
async fn failed_row_without_file_is_ignored_unless_partially_applied() {
    let m = Migrations::new(&[B]);
    let mut session = MockSession::new(true);
    session.history.insert(
        VA.to_owned(),
        row(VA, HistoryKind::Migration, HistoryStatus::Failed, "x"),
    );
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(result.unwrap(), report(&[(B.0, applied(2))]));

    let mut session = MockSession::new(true);
    let mut partial = row(VA, HistoryKind::Migration, HistoryStatus::Running, "x");
    partial.statements_applied = 1;
    session.history.insert(VA.to_owned(), partial);
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(message.contains(VA), "{message}");
}

#[tokio::test]
async fn out_of_order_files() {
    let mut m = Migrations::new(&[A, C]);
    let mut session = MockSession::new(true);
    deploy(&mut session, &m.dir, &options()).await.0.unwrap();
    m.write(B.0, B.1);
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message
            .contains("older than the newest applied version 20260103000000: 20260102000000_b.sql"),
        "{message}"
    );
    assert!(message.contains("--allow-out-of-order"), "{message}");

    let opts = DeployOptions {
        allow_out_of_order: true,
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(result.unwrap(), report(&[(B.0, applied(2))]));
}

#[tokio::test]
async fn deploy_up_to_a_version() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        to: Some(VB.to_owned()),
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(
        result.unwrap(),
        report(&[(A.0, applied(2)), (B.0, applied(2))])
    );
    assert!(!session.history.contains_key(VC));

    let opts = DeployOptions {
        to: Some("20260102000001".to_owned()),
        ..options()
    };
    let message = migrations_error(deploy(&mut session, &m.dir, &opts).await.0);
    assert!(
        message.contains("--to 20260102000001 does not match any migration file"),
        "{message}"
    );
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        dry_run: true,
        ..options()
    };
    let (result, events) = deploy(&mut session, &m.dir, &opts).await;
    let would = |statements| FileOutcome::WouldApply { statements };
    assert_eq!(
        result.unwrap(),
        report(&[(A.0, would(2)), (B.0, would(2)), (C.0, would(3))])
    );
    assert_eq!(session.log, strings(&["LOAD", "INSPECT"]));
    assert_eq!(events.len(), 3);
    assert!(session.history.is_empty());

    let opts = DeployOptions {
        dry_run: true,
        baseline: Some(VA.to_owned()),
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(
        result.unwrap(),
        report(&[
            (A.0, FileOutcome::Baselined),
            (B.0, would(2)),
            (C.0, would(3))
        ])
    );
    assert!(session.history.is_empty());
}

#[tokio::test]
async fn dry_run_of_an_interrupted_file_counts_remaining_statements() {
    let m = Migrations::new(&[C]);
    let mut session = MockSession::new(true);
    session.fail_on("i4", ExecErrorKind::Other, None);
    assert!(deploy(&mut session, &m.dir, &options()).await.0.is_err());
    let opts = DeployOptions {
        dry_run: true,
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(
        result.unwrap(),
        report(&[(C.0, FileOutcome::WouldApply { statements: 1 })])
    );
}

#[tokio::test]
async fn lock_timeouts_are_retried() {
    let m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(true);
    session.fail_on("t2", ExecErrorKind::LockTimeout, Some(2));
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(
        result.unwrap(),
        report(&[
            (
                A.0,
                FileOutcome::Applied {
                    statements: 2,
                    retries: 2
                }
            ),
            (B.0, applied(2))
        ])
    );
    let retries: Vec<_> = events.iter().filter(|e| e.starts_with("retry")).collect();
    assert_eq!(retries.len(), 2);
    assert!(
        retries[0].starts_with("retry 20260101000000 1: mock failure"),
        "{}",
        retries[0]
    );
    assert!(
        retries[1].starts_with("retry 20260101000000 2: "),
        "{}",
        retries[1]
    );
    assert_eq!(
        session.ddl(),
        [
            "CREATE TABLE t1 (id int)",
            "CREATE TABLE t2 (id int)",
            "CREATE TABLE t3 (id int)",
            "CREATE INDEX i3 ON t3 (id)"
        ]
    );
    assert_eq!(session.log.iter().filter(|l| *l == "ROLLBACK").count(), 2);
    assert_eq!(session.history[VA].status, HistoryStatus::Applied);
}

#[tokio::test]
async fn lock_timeout_retries_exhausted() {
    let m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(true);
    session.fail_on("t2", ExecErrorKind::LockTimeout, None);
    let opts = DeployOptions {
        retries: 2,
        ..options()
    };
    let (result, events) = deploy(&mut session, &m.dir, &opts).await;
    let Err(Error::Exec(error)) = result else {
        panic!("expected an exec error, got {result:?}");
    };
    assert_eq!(error.kind, ExecErrorKind::LockTimeout);
    assert!(
        error.message.contains(
            "20260101000000_a.sql:4: statement 2 of 2 failed after 2 retries: mock failure"
        ),
        "{}",
        error.message
    );
    assert_eq!(error.code.as_deref(), Some("55P03"));
    assert_eq!(events.iter().filter(|e| e.starts_with("retry")).count(), 2);
    assert!(session.ddl().is_empty());
    let a = &session.history[VA];
    assert_eq!(a.status, HistoryStatus::Failed);
    assert_eq!((a.statements_applied, a.error_statement), (0, Some(1)));
    assert!(a.error.as_deref().unwrap().contains("mock failure"));
    assert!(!session.history.contains_key(VB));
}

#[tokio::test]
async fn failing_transactional_file() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    session.fail_on("i3", ExecErrorKind::Other, None);
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    let Err(Error::Exec(error)) = result else {
        panic!("expected an exec error, got {result:?}");
    };
    assert!(
        error.message.ends_with("20260102000000_b.sql:2: statement 2 of 2 failed: mock failure: CREATE INDEX i3 ON t3 (id)"),
        "{}",
        error.message
    );
    assert_eq!(
        error.statement.as_deref(),
        Some("CREATE INDEX i3 ON t3 (id)")
    );
    assert_eq!(error.hint.as_deref(), Some("mock hint"));
    let rendered = Error::Exec(error).to_string();
    assert!(
        rendered.contains("(XX000)") && rendered.contains("hint: mock hint"),
        "{rendered}"
    );
    assert_eq!(
        session.ddl(),
        ["CREATE TABLE t1 (id int)", "CREATE TABLE t2 (id int)"]
    );
    let b = &session.history[VB];
    assert_eq!(b.status, HistoryStatus::Failed);
    assert_eq!(b.statements_applied, 0);
    assert!(b.statement_checksums.is_empty());
    assert_eq!(b.error_statement, Some(1));
    assert!(b.error.as_deref().unwrap().contains("mock failure"));
    assert!(!session.history.contains_key(VC));
    assert_eq!(
        &session.log[session.log.len() - 4..],
        strings(&[
            "CREATE INDEX i3 ON t3 (id)",
            "ROLLBACK",
            "WRITE 20260102000000 failed 0",
            "UNLOCK"
        ])
    );
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("finish 20260102000000"))
    );

    session.clear_failures();
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(
        result.unwrap(),
        report(&[(B.0, applied(2)), (C.0, applied(3))])
    );
    assert_eq!(session.history[VB].status, HistoryStatus::Applied);
}

#[tokio::test]
async fn non_transactional_failure_then_resume_after_fix() {
    let mut m = Migrations::new(&[A, C]);
    let mut session = MockSession::new(true);
    session.fail_on("i4", ExecErrorKind::Other, None);
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    let Err(Error::Exec(error)) = result else {
        panic!("expected an exec error, got {result:?}");
    };
    assert!(
        error
            .message
            .contains("20260103000000_c.sql:5: statement 3 of 3 failed"),
        "{}",
        error.message
    );
    let c = &session.history[VC];
    assert_eq!(c.status, HistoryStatus::Failed);
    assert_eq!(c.statements_applied, 2);
    assert_eq!(c.error_statement, Some(2));
    assert_eq!(
        c.statement_checksums,
        checksums(&[
            "CREATE INDEX CONCURRENTLY i1 ON t1 (id)",
            "CREATE INDEX CONCURRENTLY i2 ON t2 (id)"
        ])
    );
    assert_eq!(
        &session.ddl()[2..],
        [
            "CREATE INDEX CONCURRENTLY i1 ON t1 (id)",
            "CREATE INDEX CONCURRENTLY i2 ON t2 (id)"
        ]
    );

    m.write(C.0, &C.1.replace("i4 ON t4", "i4 ON t1"));
    session.clear_failures();
    session.take_log();
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(
        result.unwrap(),
        report(&[(
            C.0,
            FileOutcome::Resumed {
                skipped: 2,
                statements: 1
            }
        )])
    );
    assert_eq!(
        session.log,
        strings(&[
            "LOCK",
            "ENSURE",
            "LOAD",
            "WRITE 20260103000000 running 2",
            "CREATE INDEX CONCURRENTLY i4 ON t1 (id)",
            "WRITE 20260103000000 running 3",
            "WRITE 20260103000000 applied 3",
            "UNLOCK",
        ])
    );
    assert_eq!(events[0], "start 20260103000000 3");
    assert_eq!(events[1], "done 20260103000000 2");
    let c = &session.history[VC];
    assert_eq!(c.status, HistoryStatus::Applied);
    assert_eq!(c.checksum, m.file(VC).checksum);
    assert_eq!(c.statement_checksums.len(), 3);
    assert_eq!((c.error.as_deref(), c.error_statement), (None, None));
}

#[tokio::test]
async fn changed_applied_prefix_is_refused() {
    let mut m = Migrations::new(&[C]);
    let mut session = MockSession::new(true);
    session.fail_on("i4", ExecErrorKind::Other, None);
    assert!(deploy(&mut session, &m.dir, &options()).await.0.is_err());
    session.clear_failures();

    m.write(C.0, &C.1.replace("i2 ON t2", "i2 ON t3"));
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains(
            "20260103000000_c.sql: the file changed after 2 of its statements were applied"
        ),
        "{message}"
    );

    m.write(C.0, &C.1.replace("-- openmig:no-transaction\n", ""));
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains("applied without a transaction"),
        "{message}"
    );

    m.write(
        C.0,
        "-- openmig:no-transaction\nCREATE INDEX CONCURRENTLY i1 ON t1 (id);\n",
    );
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains("changed after 2 of its statements"),
        "{message}"
    );
}

#[tokio::test]
async fn engine_without_transactional_ddl() {
    let m = Migrations::new(&[A, B]);
    let mut session = MockSession::new(false);
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    assert_eq!(
        result.unwrap(),
        report(&[(A.0, applied(2)), (B.0, applied(2))])
    );
    assert!(
        !session
            .log
            .iter()
            .any(|l| l == "BEGIN" || l.starts_with("SET"))
    );
    assert!(!session.history[VA].transactional);

    let mut session = MockSession::new(false);
    session.fail_on("i3", ExecErrorKind::LockTimeout, Some(1));
    let (result, events) = deploy(&mut session, &m.dir, &options()).await;
    assert!(matches!(result, Err(Error::Exec(_))));
    assert!(!events.iter().any(|e| e.starts_with("retry")));
    assert_eq!(session.history[VB].statements_applied, 1);
}

#[tokio::test]
async fn timeout_directives() {
    let m = Migrations::new(&[
        (
            "20260101000000_a.sql",
            "-- openmig:lock-timeout 1s\n-- openmig:statement-timeout 2m\nSELECT 1;\n",
        ),
        (
            "20260102000000_b.sql",
            "-- openmig:no-transaction\n-- openmig:lock-timeout 1s\n-- openmig:statement-timeout 30s\nSELECT 2;\n",
        ),
    ]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        lock_timeout: Duration::from_secs(9),
        ..options()
    };
    deploy(&mut session, &m.dir, &opts).await.0.unwrap();
    assert_eq!(
        session.effects,
        [
            "SET LOCAL lock_timeout = '1000ms'",
            "SET statement_timeout = '120000ms'",
            "SELECT 1",
            "SET statement_timeout = '30000ms'",
            "SELECT 2"
        ]
    );
}

#[tokio::test]
async fn sum_mismatch_is_refused() {
    let m = Migrations::new(&[A, B]);
    std::fs::write(m.tmp.path().join(B.0), "SELECT 'edited';\n").unwrap();
    let dir = MigrationDir::load(m.tmp.path()).unwrap();
    let mut session = MockSession::new(true);
    let message = migrations_error(deploy(&mut session, &dir, &options()).await.0);
    assert!(message.contains("do not match openmig.sum"), "{message}");
    assert!(
        message.contains("edited: 20260102000000_b.sql"),
        "{message}"
    );
    assert!(session.effects.is_empty());
}

#[tokio::test]
async fn syntax_errors_are_reported_before_anything_runs() {
    let m = Migrations::new(&[A, ("20260102000000_b.sql", "SELECT 1;\nSELECT 'oops;\n")]);
    let mut session = MockSession::new(true);
    let message = migrations_error(deploy(&mut session, &m.dir, &options()).await.0);
    assert!(
        message.contains("20260102000000_b.sql:2: unterminated string literal"),
        "{message}"
    );
    assert!(session.effects.is_empty());
}

#[tokio::test]
async fn lock_held_elsewhere() {
    let m = Migrations::new(&[A]);
    let mut session = MockSession::new(true);
    session.lock_taken = true;
    let (result, _) = deploy(&mut session, &m.dir, &options()).await;
    assert!(matches!(result, Err(Error::LockTimeout(_))));
    assert_eq!(session.log, strings(&["LOCK"]));
}

#[tokio::test]
async fn without_history() {
    let m = Migrations::new(&[A]);
    let mut session = MockSession::new(true);
    let opts = DeployOptions {
        record_history: false,
        ..options()
    };
    let (result, _) = deploy(&mut session, &m.dir, &opts).await;
    assert_eq!(result.unwrap(), report(&[(A.0, applied(2))]));
    assert!(session.history.is_empty());
    assert!(!session.log.iter().any(|l| l == "ENSURE" || l == "LOAD"));
}

#[tokio::test]
async fn replay_applies_everything() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    openmig_migrate::replay::<MockEngine>(&mut session, &m.dir)
        .await
        .unwrap();
    assert_eq!(
        session.log,
        strings(&[
            "BEGIN",
            "CREATE TABLE t1 (id int)",
            "CREATE TABLE t2 (id int)",
            "COMMIT",
            "RESET",
            "BEGIN",
            "CREATE TABLE t3 (id int)",
            "CREATE INDEX i3 ON t3 (id)",
            "COMMIT",
            "RESET",
            "CREATE INDEX CONCURRENTLY i1 ON t1 (id)",
            "CREATE INDEX CONCURRENTLY i2 ON t2 (id)",
            "CREATE INDEX CONCURRENTLY i4 ON t4 (id)",
        ])
    );
    assert!(session.history.is_empty());
}

#[tokio::test]
async fn replay_error_location() {
    let m = Migrations::new(&[A, B, C]);
    let mut session = MockSession::new(true);
    session.fail_at(3, ExecErrorKind::UndefinedObject);
    let result = openmig_migrate::replay::<MockEngine>(&mut session, &m.dir).await;
    let Err(Error::Exec(error)) = result else {
        panic!("expected an exec error, got {result:?}");
    };
    let path = m.tmp.path().join(B.0);
    assert_eq!(
        error.message,
        format!(
            "{}:2: statement 2 of 2 failed: mock failure: CREATE INDEX i3 ON t3 (id)",
            path.display()
        )
    );
    assert_eq!(error.kind, ExecErrorKind::UndefinedObject);
    assert_eq!(error.code.as_deref(), Some("XX000"));
    assert_eq!(
        session.ddl(),
        ["CREATE TABLE t1 (id int)", "CREATE TABLE t2 (id int)"]
    );
    assert!(!session.in_transaction());

    let mut session = MockSession::new(true);
    session.fail_on("i2", ExecErrorKind::Other, None);
    let result = openmig_migrate::replay::<MockEngine>(&mut session, &m.dir).await;
    let Err(Error::Exec(error)) = result else {
        panic!("expected an exec error, got {result:?}");
    };
    assert!(
        error
            .message
            .contains("20260103000000_c.sql:4: statement 2 of 3 failed"),
        "{}",
        error.message
    );
}

#[test]
fn status_states() {
    let m = Migrations::new(&[
        ("20260101000000_a.sql", "SELECT 1;"),
        ("20260102000000_b.sql", "SELECT 2;"),
        ("20260103000000_c.sql", "SELECT 3;"),
        ("20260104000000_d.sql", "SELECT 4;"),
        ("20260105000000_e.sql", "SELECT 5;"),
        ("20260106000000_f.sql", "SELECT 6;"),
        ("20260107000000_g.sql", "SELECT 7;"),
    ]);
    let checksum = |v: &str| m.file(v).checksum.clone();
    let mut gone = row(
        "20251231000000",
        HistoryKind::Migration,
        HistoryStatus::Applied,
        "x",
    );
    gone.name = "gone".to_owned();
    let mut failed = row(
        "20260106000000",
        HistoryKind::Migration,
        HistoryStatus::Failed,
        "y",
    );
    failed.error = Some("boom".to_owned());
    let history = vec![
        gone,
        row(
            VA,
            HistoryKind::Migration,
            HistoryStatus::Applied,
            &checksum(VA),
        ),
        row(
            VB,
            HistoryKind::Baseline,
            HistoryStatus::Applied,
            &checksum(VB),
        ),
        row(
            VC,
            HistoryKind::Migration,
            HistoryStatus::Applied,
            "changed",
        ),
        row(
            "20260105000000",
            HistoryKind::Migration,
            HistoryStatus::Applied,
            &checksum("20260105000000"),
        ),
        failed,
    ];
    let status = openmig_migrate::status(&history, &m.dir);
    let got: Vec<_> = status
        .entries
        .iter()
        .map(|e| (e.version.as_str(), e.name.as_str(), e.state.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("20251231000000", "gone", StatusState::Missing),
            (VA, "a", StatusState::Applied),
            (VB, "b", StatusState::Baseline),
            (VC, "c", StatusState::Modified),
            ("20260104000000", "d", StatusState::OutOfOrder),
            ("20260105000000", "e", StatusState::Applied),
            (
                "20260106000000",
                "f",
                StatusState::Failed {
                    error: Some("boom".to_owned())
                }
            ),
            ("20260107000000", "g", StatusState::Pending),
        ]
    );
    let pending: Vec<_> = status.pending().map(|e| e.name.as_str()).collect();
    assert_eq!(pending, ["d", "f", "g"]);

    let fresh = openmig_migrate::status(&[], &m.dir);
    assert!(
        fresh
            .entries
            .iter()
            .all(|e| e.state == StatusState::Pending)
    );
}

#[test]
fn deploy_future_is_send() {
    fn assert_send<T: Send>(_: T) {}
    let m = Migrations::new(&[]);
    let mut session = MockSession::new(true);
    let mut events = Events::default();
    let options = options();
    assert_send(openmig_migrate::deploy::<MockEngine>(
        &mut session,
        &m.dir,
        &options,
        &mut events,
    ));
    assert_send(openmig_migrate::replay::<MockEngine>(&mut session, &m.dir));
}
