//! Sessions on a real server: history, the KeeperMap lock, reset and error mapping.

mod common;

use common::{connect, run};
use jiff::Timestamp;
use openmig_clickhouse::{ChSettings, ClickHouseEngine};
use openmig_core::{
    Engine, Error, ExecErrorKind, HistoryKind, HistoryRow, HistoryStatus, Scope, Session,
    SessionPurpose,
};
use pretty_assertions::assert_eq;
use std::time::Duration;

fn row(version: &str, status: HistoryStatus) -> HistoryRow {
    HistoryRow {
        version: version.into(),
        name: "add_events".into(),
        checksum: "ab".repeat(32),
        kind: HistoryKind::Migration,
        transactional: false,
        statements_total: 3,
        statements_applied: 1,
        statement_checksums: vec!["c1".into(), "it's \\ quoted".into()],
        status,
        error: Some("Code: 60. it's broken".into()),
        error_statement: Some(1),
        started_at: Timestamp::from_millisecond(1_793_000_000_123).unwrap(),
        finished_at: None,
        applied_by: "openmig/0.1.0 host=test user=default".into(),
    }
}

async fn history_round_trip(settings: &ChSettings, replicated: bool) {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, settings).await;
    assert_eq!(session.history_load().await.unwrap(), vec![]);
    session.history_delete("20260101000000").await.unwrap();
    session.history_ensure().await.unwrap();
    session.history_ensure().await.unwrap();
    assert_eq!(session.history_load().await.unwrap(), vec![]);

    let mut first = row("20260101000000", HistoryStatus::Running);
    session.history_write(&first).await.unwrap();
    first.status = HistoryStatus::Failed;
    first.statements_applied = 2;
    session.history_write(&first).await.unwrap();
    let mut second = row("20260102000000", HistoryStatus::Applied);
    second.kind = HistoryKind::Baseline;
    second.transactional = true;
    second.error = None;
    second.error_statement = None;
    second.statement_checksums = vec![];
    second.finished_at = Some(Timestamp::from_millisecond(1_793_000_001_456).unwrap());
    session.history_write(&second).await.unwrap();
    assert_eq!(
        session.history_load().await.unwrap(),
        vec![first.clone(), second.clone()]
    );

    session.history_delete(&first.version).await.unwrap();
    assert_eq!(session.history_load().await.unwrap(), vec![second]);

    let history = settings.history().unwrap();
    let engine = lease
        .query(&format!(
            "SELECT engine FROM system.tables WHERE database = '{}' AND name = '{}'",
            history.database, history.name
        ))
        .await
        .unwrap();
    assert_eq!(
        engine.trim().starts_with("Replicated"),
        replicated,
        "{engine}"
    );

    let model = session
        .inspect(&Scope {
            manage_roles: true,
            ..Scope::default()
        })
        .await
        .unwrap();
    assert!(!model.databases.contains_key(&history.database));
    assert!(ClickHouseEngine::is_empty(&model));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history() {
    history_round_trip(&ChSettings::default(), false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_history() {
    let settings = ChSettings {
        cluster: Some("{cluster}".into()),
        history_table: "ops.migrations".into(),
        ..ChSettings::default()
    };
    history_round_trip(&settings, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeper_map_lock() {
    let lease = openmig_testkit::clickhouse().await;
    let settings = ChSettings::default();
    let mut first = connect(&lease, &settings).await;
    let mut second = connect(&lease, &settings).await;

    first.lock(Duration::from_secs(1)).await.unwrap();
    first.lock(Duration::from_secs(1)).await.unwrap();
    let started = std::time::Instant::now();
    let error = second.lock(Duration::from_millis(600)).await.unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(600));
    let Error::LockTimeout(message) = &error else {
        panic!("expected a lock timeout, got {error}");
    };
    assert!(message.contains("held by `openmig/"), "{message}");
    assert!(
        message.contains("0s ago") || message.contains("1s ago"),
        "{message}"
    );
    assert_eq!(error.exit_code(), 5);

    first.unlock().await.unwrap();
    first.unlock().await.unwrap();
    second.lock(Duration::from_secs(1)).await.unwrap();
    first.unlock().await.unwrap();
    first.lock(Duration::from_millis(300)).await.unwrap_err();
    second.unlock().await.unwrap();

    lease
        .query(
            "INSERT INTO openmig.lock SETTINGS async_insert = 0 \
             VALUES ('lock', 'ghost-deploy', now64(3) - INTERVAL 2 HOUR)",
        )
        .await
        .unwrap();
    let error = first.lock(Duration::from_millis(300)).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("ghost-deploy"), "{message}");
    let age: u64 = message
        .split("s ago")
        .next()
        .and_then(|before| before.rsplit('(').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no age in {message}"));
    assert!((7200..7300).contains(&age), "{message}");
    assert!(
        message.contains("DELETE FROM `openmig`.`lock`"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_drops_everything() {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, &ChSettings::default()).await;
    run(
        &mut session,
        "CREATE DATABASE app ENGINE = Replicated('/clickhouse/databases/app', '{shard}', '{replica}');
         CREATE TABLE app.t (id UInt64) ENGINE = ReplicatedMergeTree ORDER BY id;
         CREATE TABLE default.t (id UInt64) ENGINE = MergeTree ORDER BY id;
         CREATE ROLE r;
         CREATE ROLE rr IN replicated;
         CREATE USER u IDENTIFIED WITH no_password;
         CREATE FUNCTION f AS x -> x + 1;
         CREATE ROW POLICY p ON default.t USING id > 1 TO r;
         CREATE QUOTA q FOR INTERVAL 1 hour MAX queries = 10 TO r;
         CREATE SETTINGS PROFILE sp SETTINGS max_threads = 1 TO r;
         CREATE NAMED COLLECTION nc AS key = 'value';",
    )
    .await;
    session.history_ensure().await.unwrap();
    session.lock(Duration::from_secs(1)).await.unwrap();

    session.reset().await.unwrap();
    let model = session
        .inspect(&Scope {
            manage_roles: true,
            ..Scope::default()
        })
        .await
        .unwrap();
    assert_eq!(model.databases.keys().collect::<Vec<_>>(), ["default"]);
    assert!(ClickHouseEngine::is_empty(&model));
    for (table, filter) in [
        ("quotas", "storage != 'users_xml'"),
        ("settings_profiles", "storage != 'users_xml'"),
        ("row_policies", "storage != 'users_xml'"),
        ("named_collections", "1"),
    ] {
        let count = lease
            .query(&format!(
                "SELECT count() FROM system.{table} WHERE {filter}"
            ))
            .await
            .unwrap();
        assert_eq!(count.trim(), "0", "{table}");
    }

    // The same names can be created again, including the replicated database path.
    run(
        &mut session,
        "CREATE DATABASE app ENGINE = Replicated('/clickhouse/databases/app', '{shard}', '{replica}');
         CREATE ROLE rr IN replicated;",
    )
    .await;
    session.lock(Duration::from_secs(1)).await.unwrap();

    let mut target =
        ClickHouseEngine::connect(lease.url(), &ChSettings::default(), SessionPurpose::Target)
            .await
            .unwrap();
    assert!(target.reset().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_and_sessions() {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, &ChSettings::default()).await;
    assert!(!session.capabilities().transactional_ddl);
    assert!(!session.capabilities().savepoints);
    assert_eq!(session.server_version().major, 26);
    assert!(session.begin().await.is_err());
    assert!(session.commit().await.is_err());
    assert!(session.rollback().await.is_err());

    let cases = [
        (
            "SELECT * FROM nope.t",
            "UNKNOWN_DATABASE",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "SELECT * FROM default.nope",
            "UNKNOWN_TABLE",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "SELECT nope_fn(1)",
            "UNKNOWN_FUNCTION",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "SELECT nope_column",
            "UNKNOWN_IDENTIFIER",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "GRANT SELECT ON *.* TO nope_role",
            "UNKNOWN_ROLE",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "DROP USER nope_user",
            "UNKNOWN_USER",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "CREATE VIEW default.v AS SELECT * FROM default.missing",
            "UNKNOWN_TABLE",
            ExecErrorKind::UndefinedObject,
        ),
        (
            "CREATE TABLE default.t ON CLUSTER '{cluster}' AS default.missing",
            "CANNOT_GET_CREATE_TABLE_QUERY",
            ExecErrorKind::UndefinedObject,
        ),
        ("SELEC 1", "SYNTAX_ERROR", ExecErrorKind::Other),
        (
            "CREATE DATABASE default",
            "DATABASE_ALREADY_EXISTS",
            ExecErrorKind::Other,
        ),
    ];
    for (sql, code, kind) in cases {
        let error = session.execute(sql).await.unwrap_err();
        assert_eq!(error.code.as_deref(), Some(code), "{sql}: {error}");
        assert_eq!(error.kind, kind, "{sql}: {error}");
        assert_eq!(error.statement.as_deref(), Some(sql));
        assert!(!error.message.contains("(version"), "{}", error.message);
    }

    // `SET` lasts for the HTTP session until `reset_session`.
    session
        .execute("SET allow_experimental_time_series_table = 0")
        .await
        .unwrap();
    let error = session
        .execute("CREATE TABLE default.ts ENGINE = TimeSeries")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ExecErrorKind::Other, "{error}");
    session.reset_session().await.unwrap();
    session
        .execute("CREATE TABLE default.ts ENGINE = TimeSeries")
        .await
        .unwrap();

    let closed = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let error = ClickHouseEngine::connect(
        &format!("http://default:secret@127.0.0.1:{closed}/"),
        &ChSettings::default(),
        SessionPurpose::Target,
    )
    .await
    .err()
    .expect("connecting to a closed port fails");
    assert!(matches!(error, Error::Connect { .. }), "{error}");
    assert!(!error.to_string().contains("secret"), "{error}");

    let error = ClickHouseEngine::connect(
        &format!(
            "http://nobody:wrong@{}/",
            lease
                .url()
                .trim_start_matches("http://default@")
                .trim_end_matches('/')
        ),
        &ChSettings::default(),
        SessionPurpose::Target,
    )
    .await
    .err()
    .expect("a wrong password fails");
    assert!(matches!(error, Error::Connect { .. }), "{error}");
}
