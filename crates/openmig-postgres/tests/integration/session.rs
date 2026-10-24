use crate::common::{ROLES, TestDb, admin_url, connect, drop_role, url_for};
use jiff::Timestamp;
use openmig_core::{
    Engine, Error, ExecErrorKind, HistoryKind, HistoryRow, HistoryStatus, Scope, Session,
    SessionPurpose,
};
use openmig_postgres::{PgSettings, PostgresEngine, SCRATCH_DATABASE};
use std::time::{Duration, Instant};

#[tokio::test]
async fn version_and_capabilities() {
    let Some(db) = TestDb::create("omt_version").await else {
        return;
    };
    let session = PostgresEngine::connect(&db.url, &PgSettings::default(), SessionPurpose::Target)
        .await
        .unwrap();
    let version = session.server_version();
    assert!(version.major >= 14, "{version:?}");
    assert!(version.text.starts_with(&version.major.to_string()));
    assert!(session.capabilities().transactional_ddl);
    assert!(session.capabilities().savepoints);
    drop(session);
    db.drop().await;
}

#[tokio::test]
async fn connection_errors_are_reported() {
    let Some(admin) = admin_url() else {
        return;
    };
    let url = url_for(&admin, "omt_does_not_exist");
    let err = PostgresEngine::connect(&url, &PgSettings::default(), SessionPurpose::Target)
        .await
        .err()
        .expect("connecting to a missing database fails");
    assert!(matches!(err, Error::Connect { .. }), "{err:?}");
    assert!(err.to_string().contains("omt_does_not_exist"), "{err}");
}

fn row(version: &str, status: HistoryStatus) -> HistoryRow {
    HistoryRow {
        version: version.into(),
        name: "add_users".into(),
        checksum: "abc".into(),
        kind: HistoryKind::Migration,
        transactional: false,
        statements_total: 3,
        statements_applied: 1,
        statement_checksums: vec!["s1".into()],
        status,
        error: None,
        error_statement: None,
        started_at: "2026-10-24T12:00:00.123456Z".parse::<Timestamp>().unwrap(),
        finished_at: None,
        applied_by: "openmig/test".into(),
    }
}

#[tokio::test]
async fn history_round_trip() {
    let Some(db) = TestDb::create("omt_history").await else {
        return;
    };
    let mut s = db.session().await;
    assert!(s.history_load().await.unwrap().is_empty());
    s.history_ensure().await.unwrap();
    s.history_ensure().await.unwrap();
    assert!(s.history_load().await.unwrap().is_empty());

    let first = row("20261024120000", HistoryStatus::Running);
    s.history_write(&first).await.unwrap();
    let mut second = row("20261024110000", HistoryStatus::Applied);
    second.kind = HistoryKind::Baseline;
    second.name = String::new();
    s.history_write(&second).await.unwrap();
    assert_eq!(
        s.history_load().await.unwrap(),
        vec![second.clone(), first.clone()]
    );

    let mut updated = first.clone();
    updated.status = HistoryStatus::Failed;
    updated.statements_applied = 2;
    updated.statement_checksums = vec!["s1".into(), "s2".into()];
    updated.error = Some("boom".into());
    updated.error_statement = Some(2);
    updated.finished_at = Some("2026-10-24T12:00:05Z".parse().unwrap());
    s.history_write(&updated).await.unwrap();
    assert_eq!(
        s.history_load().await.unwrap(),
        vec![second.clone(), updated]
    );

    s.begin().await.unwrap();
    s.history_delete("20261024120000").await.unwrap();
    s.rollback().await.unwrap();
    assert_eq!(s.history_load().await.unwrap().len(), 2);
    s.history_delete("20261024120000").await.unwrap();
    assert_eq!(s.history_load().await.unwrap(), vec![second]);

    let model = s.inspect(&Scope::default()).await.unwrap();
    assert!(
        !model.schemas.contains_key("openmig"),
        "history schema is never inspected"
    );
    let comment: String = s
        .client()
        .query_one(
            "SELECT obj_description('openmig.history'::regclass, 'pg_class')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(comment, "openmig-history v1");
    drop(s);
    db.drop().await;
}

#[tokio::test]
async fn advisory_lock() {
    let Some(db) = TestDb::create("omt_lock").await else {
        return;
    };
    let mut a = db.session().await;
    let mut b = db.session().await;
    a.lock(Duration::from_secs(1)).await.unwrap();
    let started = Instant::now();
    let err = b.lock(Duration::from_millis(400)).await.unwrap_err();
    assert!(matches!(err, Error::LockTimeout(_)), "{err:?}");
    assert!(started.elapsed() >= Duration::from_millis(400));
    a.unlock().await.unwrap();
    b.lock(Duration::from_secs(1)).await.unwrap();
    b.unlock().await.unwrap();
    drop((a, b));
    db.drop().await;
}

#[tokio::test]
async fn error_mapping() {
    let Some(db) = TestDb::create("omt_errors").await else {
        return;
    };
    let mut s = db.session().await;
    let e = s.execute("SELECT * FROM missing_table").await.unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::UndefinedObject);
    assert_eq!(e.code.as_deref(), Some("42P01"));
    assert_eq!(e.position, Some(15));
    assert_eq!(e.statement.as_deref(), Some("SELECT * FROM missing_table"));

    let e = s.execute("SELECT missing_function(1)").await.unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::UndefinedObject);
    assert_eq!(e.code.as_deref(), Some("42883"));
    assert!(e.hint.is_some());

    let e = s
        .execute("CREATE TABLE missing_schema.t (id int)")
        .await
        .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::UndefinedObject);
    assert_eq!(e.code.as_deref(), Some("3F000"));

    let e = s.execute("SELEC 1").await.unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Other);
    assert_eq!(e.code.as_deref(), Some("42601"));

    s.execute("CREATE TABLE public.t (id int PRIMARY KEY)")
        .await
        .unwrap();
    s.execute("INSERT INTO public.t VALUES (1)").await.unwrap();
    let e = s
        .execute("INSERT INTO public.t VALUES (1)")
        .await
        .unwrap_err();
    assert_eq!(e.code.as_deref(), Some("23505"));
    assert!(e.detail.as_deref().unwrap_or_default().contains("(id)=(1)"));

    let mut other = db.session().await;
    s.begin().await.unwrap();
    s.execute("LOCK TABLE public.t IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    other.begin().await.unwrap();
    let timeout = PostgresEngine::lock_timeout_sql(Duration::from_millis(100)).unwrap();
    other.execute(&timeout).await.unwrap();
    let e = other.execute("SELECT * FROM public.t").await.unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::LockTimeout);
    other.rollback().await.unwrap();
    s.rollback().await.unwrap();

    s.execute("SET search_path = pg_catalog").await.unwrap();
    s.reset_session().await.unwrap();
    let path: String = s
        .client()
        .query_one("SELECT current_setting('search_path')", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(path, "\"$user\", public");
    drop((s, other));
    db.drop().await;
}

#[tokio::test]
async fn scratch_reset() {
    let _roles = ROLES.write().await;
    let Some(admin) = admin_url() else {
        return;
    };
    drop_role("omt_scratch_role").await;
    let url = url_for(&admin, "omt_scratch");
    let mut s = connect(&url, SessionPurpose::Scratch).await;
    assert_eq!(s.database(), "omt_scratch");
    s.execute(
        "CREATE ROLE omt_scratch_role; CREATE SCHEMA app; CREATE TABLE app.t (id int); \
         GRANT SELECT ON app.t TO omt_scratch_role",
    )
    .await
    .unwrap();
    assert!(!s.inspect(&Scope::default()).await.unwrap().is_empty());

    s.reset().await.unwrap();
    assert_eq!(s.database(), "omt_scratch");
    let model = s.inspect(&Scope::default()).await.unwrap();
    assert!(model.is_empty(), "{model:#?}");
    let roles: i64 = s
        .client()
        .query_one(
            "SELECT count(*) FROM pg_roles WHERE rolname = 'omt_scratch_role'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(roles, 0);

    let mut target = connect(&url, SessionPurpose::Target).await;
    assert!(target.reset().await.is_err());
    drop(target);

    let mut default = connect(&admin, SessionPurpose::Scratch).await;
    assert_eq!(default.database(), SCRATCH_DATABASE);
    default
        .execute("CREATE TABLE public.x (id int)")
        .await
        .unwrap();
    drop(default);
    let mut default = connect(&admin, SessionPurpose::Scratch).await;
    assert!(default.inspect(&Scope::default()).await.unwrap().is_empty());
    drop((s, default));

    let mut cleanup = connect(&admin, SessionPurpose::Target).await;
    for db in ["omt_scratch", SCRATCH_DATABASE] {
        cleanup
            .execute(&format!("DROP DATABASE {db} WITH (FORCE)"))
            .await
            .unwrap();
    }
}
