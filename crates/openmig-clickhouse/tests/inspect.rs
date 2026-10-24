//! Inspection of a real server: a rich schema shaped like a production analytics setup
//! (Replicated databases, replicated MergeTree engines, TimeSeries, codecs, TTLs, skip indexes,
//! projections, materialized views with `TO`, roles, grants and row policies).

mod common;

use common::{compact, connect, run};
use openmig_clickhouse::model::{GrantLevel, Grantee, Model};
use openmig_clickhouse::{ChSettings, ClickHouseEngine};
use openmig_core::{Engine, ExcludeSet, ObjectKind, ObjectRef, SchemaFilter, Scope, Session};
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;

const DATABASES: &[&str] = &[
    "CREATE DATABASE analytics ON CLUSTER '{cluster}' \
     ENGINE = Replicated('/clickhouse/databases/analytics', '{shard}', '{replica}') \
     COMMENT 'product analytics'",
    "CREATE DATABASE staging ON CLUSTER '{cluster}'",
];

const ACCESS: &[&str] = &[
    "CREATE ROLE reader ON CLUSTER '{cluster}'",
    "CREATE ROLE admin ON CLUSTER '{cluster}' IN replicated",
    "CREATE ROLE ingest ON CLUSTER '{cluster}'",
    "CREATE ROLE gateway ON CLUSTER '{cluster}'",
    "CREATE USER app ON CLUSTER '{cluster}' IDENTIFIED WITH sha256_password BY 'secret' \
     HOST IP '10.0.0.0/8' DEFAULT DATABASE analytics",
    "CREATE FUNCTION ms_to_s ON CLUSTER '{cluster}' AS (ms) -> ms / 1000",
];

const TABLES: &[&str] = &[
    "CREATE TABLE analytics.events (
       call_id String,
       offset_ms Float64,
       timestamp DateTime64(9),
       kind LowCardinality(String),
       payload String COMMENT 'raw JSON',
       INDEX call_id_set call_id TYPE set(1024)
     ) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')
     PRIMARY KEY timestamp ORDER BY timestamp PARTITION BY toYYYYMM(timestamp)
     SETTINGS index_granularity = 8192, storage_policy = 'encrypted'",
    "CREATE TABLE analytics.app_logs (
       ts DateTime64(9) CODEC(Delta(8), ZSTD(1)),
       ts_time DateTime DEFAULT toDateTime(ts),
       trace_id String CODEC(ZSTD(1)),
       severity Enum8('debug' = 1, 'info' = 2, 'warn' = 3, 'error' = 4),
       service LowCardinality(String) CODEC(ZSTD(1)),
       body String CODEC(ZSTD(1)),
       attributes Map(LowCardinality(String), String) CODEC(ZSTD(1)),
       user_id Nullable(UInt64) TTL ts_time + INTERVAL 30 DAY,
       day Date MATERIALIZED toDate(ts),
       service_upper String ALIAS upper(service),
       raw String EPHEMERAL,
       INDEX idx_body body TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 8,
       INDEX idx_attr_keys mapKeys(attributes) TYPE bloom_filter(0.01),
       INDEX idx_service service TYPE ngrambf_v1(3, 256, 2, 0) GRANULARITY 2,
       INDEX idx_ts ts TYPE minmax,
       CONSTRAINT known_severity CHECK severity != 'debug',
       CONSTRAINT has_service ASSUME service != '',
       PROJECTION by_service (SELECT service, count() GROUP BY service)
     ) ENGINE = ReplicatedMergeTree
     PARTITION BY toDate(ts_time) PRIMARY KEY (service, ts_time) ORDER BY (service, ts_time, ts)
     TTL ts_time + INTERVAL 180 DAY
     SETTINGS storage_policy = 'encrypted', ttl_only_drop_parts = 1",
    "CREATE TABLE analytics.spans (
       trace_id String CODEC(ZSTD(1)),
       start DateTime64(9),
       duration_ns UInt64,
       links Array(Tuple(trace_id String, span_id String)),
       updated_at DateTime64(6, 'UTC')
     ) ENGINE = ReplicatedReplacingMergeTree(updated_at)
     ORDER BY (trace_id, toDateTime(start))
     TTL toDateTime(start) + INTERVAL 7 DAY DELETE, toDateTime(start) + INTERVAL 1 DAY TO DISK 'default'",
    "CREATE TABLE analytics.trace_bounds (
       trace_id String CODEC(ZSTD(1)),
       first DateTime64(9) CODEC(Delta(8), ZSTD(1)),
       last DateTime64(9) CODEC(Delta(8), ZSTD(1))
     ) ENGINE = ReplicatedMergeTree ORDER BY trace_id",
    "CREATE TABLE analytics.metrics ENGINE = TimeSeries",
    "CREATE TABLE staging.raw ON CLUSTER '{cluster}' (
       id UInt64,
       value String DEFAULT '',
       at DateTime,
       secs Float64 DEFAULT ms_to_s(id)
     ) ENGINE = MergeTree ORDER BY id SAMPLE BY id SETTINGS index_granularity = 1024
     COMMENT 'landing table'",
];

const VIEWS: &[&str] = &[
    "CREATE MATERIALIZED VIEW analytics.trace_bounds_mv TO analytics.trace_bounds AS
     SELECT trace_id, min(start) AS first, max(start) AS last
     FROM analytics.spans WHERE trace_id != '' GROUP BY trace_id",
    "CREATE VIEW analytics.error_logs AS
     SELECT ts, service, ms_to_s(toUnixTimestamp64Milli(ts)) AS s, body
     FROM analytics.app_logs WHERE severity = 'error'",
];

const GRANTS: &[&str] = &[
    "GRANT ON CLUSTER '{cluster}' SELECT ON analytics.* TO reader",
    "GRANT ON CLUSTER '{cluster}' ALL ON analytics.* TO admin WITH GRANT OPTION",
    "GRANT ON CLUSTER '{cluster}' SELECT(trace_id, start), INSERT(trace_id) ON analytics.spans TO ingest",
    "GRANT ON CLUSTER '{cluster}' INSERT ON staging.raw TO ingest",
    "GRANT ON CLUSTER '{cluster}' ALL ON analytics.events TO gateway WITH GRANT OPTION",
    "GRANT ON CLUSTER '{cluster}' SELECT(call_id) ON analytics.events TO gateway",
    "GRANT ON CLUSTER '{cluster}' reader TO app",
    "GRANT ON CLUSTER '{cluster}' reader TO admin WITH ADMIN OPTION",
    "ALTER USER app ON CLUSTER '{cluster}' DEFAULT ROLE reader",
    "CREATE ROW POLICY kind_a ON CLUSTER '{cluster}' ON analytics.events \
     FOR SELECT USING kind = 'a' TO reader",
];

fn script(reverse: bool) -> String {
    let mut out = Vec::new();
    for group in [DATABASES, ACCESS, TABLES, VIEWS, GRANTS] {
        let mut statements: Vec<&str> = group.to_vec();
        if reverse && group != DATABASES {
            statements.reverse();
        }
        if reverse && group == GRANTS {
            // `DEFAULT ROLE reader` needs the role granted first.
            statements.retain(|s| !s.starts_with("ALTER USER"));
            statements.push(GRANTS.iter().find(|s| s.starts_with("ALTER USER")).unwrap());
        }
        out.extend(statements);
    }
    out.join(";\n")
}

fn all() -> Scope {
    Scope {
        manage_roles: true,
        ..Scope::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rich_schema() {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, &ChSettings::default()).await;

    let pristine = session.inspect(&all()).await.unwrap();
    assert!(ClickHouseEngine::is_empty(&pristine));
    assert_eq!(pristine.databases.keys().collect::<Vec<_>>(), ["default"]);

    run(&mut session, &script(false)).await;
    let model = session.inspect(&all()).await.unwrap();
    insta::assert_snapshot!("rich_schema", compact(&model));
    check_model(&model);
    assert_eq!(session.inspect(&all()).await.unwrap(), model);

    filters(&mut session, &model).await;

    session.reset().await.unwrap();
    assert_eq!(session.inspect(&all()).await.unwrap(), pristine);
    run(&mut session, &script(true)).await;
    let reordered = session.inspect(&all()).await.unwrap();
    assert_eq!(compact(&reordered), compact(&model));
    assert_eq!(reordered, model);
}

fn check_model(model: &Model) {
    assert!(!ClickHouseEngine::is_empty(model));
    let analytics = &model.databases["analytics"];
    assert!(analytics.is_replicated());
    let logs = &analytics.tables["app_logs"];
    assert_eq!(
        logs.engine,
        "ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')"
    );
    assert_eq!(logs.primary_key, ["service", "ts_time"]);
    assert_eq!(logs.order_by, ["service", "ts_time", "ts"]);
    assert_eq!(logs.settings["index_granularity"], "8192");
    assert_eq!(logs.settings["storage_policy"], "'encrypted'");
    assert_eq!(logs.indexes.len(), 4);
    assert_eq!(
        logs.column("user_id").unwrap().ttl.as_deref(),
        Some("ts_time + toIntervalDay(30)")
    );
    assert!(analytics.tables["metrics"].columns.is_empty());
    assert!(analytics.tables["metrics"].time_series.is_some());
    assert!(
        analytics
            .tables
            .keys()
            .all(|name| !name.starts_with(".inner"))
    );
    let mv = &analytics.materialized_views["trace_bounds_mv"];
    assert_eq!(
        mv.to.as_ref().unwrap().to_string(),
        "analytics.trace_bounds"
    );

    let admin = model
        .grants
        .iter()
        .find(|g| g.grantee == Grantee::Role("admin".into()))
        .unwrap();
    assert_eq!(admin.level(), GrantLevel::Database);
    assert_eq!(admin.privileges, BTreeSet::from(["ALL".to_owned()]));
    assert!(admin.grant_option);
    let gateway: Vec<_> = model
        .grants
        .iter()
        .filter(|g| g.grantee == Grantee::Role("gateway".into()))
        .collect();
    assert_eq!(gateway.len(), 1, "{gateway:?}");
    assert_eq!(gateway[0].level(), GrantLevel::Table);
    assert_eq!(gateway[0].privileges, BTreeSet::from(["ALL".to_owned()]));
    assert!(gateway[0].grant_option);
    assert!(model.users.contains_key("app"));
    assert!(!model.users.contains_key("default"));
    assert_eq!(model.roles["admin"].storage, "replicated");
    assert_eq!(model.roles["reader"].storage, "local_directory");

    let has = |from: (ObjectKind, &[&str]), to: (ObjectKind, &[&str])| {
        model.dependencies.iter().any(|d| {
            d.from == ObjectRef::new(from.0, from.1.iter().copied())
                && d.to == ObjectRef::new(to.0, to.1.iter().copied())
        })
    };
    let mv_ref = (
        ObjectKind::MaterializedView,
        &["analytics", "trace_bounds_mv"][..],
    );
    assert!(has(
        mv_ref,
        (ObjectKind::Table, &["analytics", "trace_bounds"])
    ));
    assert!(has(mv_ref, (ObjectKind::Table, &["analytics", "spans"])));
    let view = (ObjectKind::View, &["analytics", "error_logs"][..]);
    assert!(has(view, (ObjectKind::Table, &["analytics", "app_logs"])));
    // The server stores view queries and column expressions with SQL functions inlined.
    assert!(!analytics.views["error_logs"].query.contains("ms_to_s"));
    let secs = model.databases["staging"].tables["raw"]
        .column("secs")
        .unwrap();
    assert_eq!(secs.default.as_ref().unwrap().expression, "id / 1000");
}

async fn filters(session: &mut openmig_clickhouse::ChSession, full: &Model) {
    let only = session
        .inspect(&Scope {
            schemas: SchemaFilter::Only(vec!["staging".into()]),
            manage_roles: true,
            ..Scope::default()
        })
        .await
        .unwrap();
    assert_eq!(only.databases.keys().collect::<Vec<_>>(), ["staging"]);
    assert!(
        only.grants
            .iter()
            .all(|g| g.database.as_deref() != Some("analytics"))
    );
    assert!(only.row_policies.is_empty());
    assert!(only.dependencies.is_empty());

    let excluded = session
        .inspect(&Scope {
            exclude: ExcludeSet::new([
                "table:analytics.spans",
                "column:analytics.app_logs.body",
                "index:analytics.app_logs.idx_ts",
                "role:ingest",
                "function:*",
                "default",
            ])
            .unwrap(),
            manage_roles: true,
            ..Scope::default()
        })
        .await
        .unwrap();
    let analytics = &excluded.databases["analytics"];
    assert!(!analytics.tables.contains_key("spans"));
    assert!(analytics.tables["app_logs"].column("body").is_none());
    assert!(!analytics.tables["app_logs"].indexes.contains_key("idx_ts"));
    assert!(!excluded.databases.contains_key("default"));
    assert!(!excluded.roles.contains_key("ingest"));
    assert!(
        excluded
            .grants
            .iter()
            .all(|g| g.grantee != Grantee::Role("ingest".into()))
    );
    assert!(excluded.functions.is_empty());
    assert!(excluded.dependencies.iter().all(|d| {
        d.to.path
            .last()
            .is_some_and(|n| n != "spans" && n != "ms_to_s")
    }));

    let unmanaged = session
        .inspect(&Scope {
            declared_roles: vec!["reader".into()],
            ..Scope::default()
        })
        .await
        .unwrap();
    assert!(unmanaged.roles.is_empty() && unmanaged.users.is_empty());
    assert!(unmanaged.role_grants.is_empty());
    assert!(!unmanaged.grants.is_empty());
    assert!(
        unmanaged
            .grants
            .iter()
            .all(|g| g.grantee == Grantee::Role("reader".into()))
    );
    assert_eq!(unmanaged.databases, full.databases);
}

/// The server keeps parentheses written around whole expressions; tables written with them (as
/// generated migrations often are) and without them must inspect equal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redundant_parentheses_inspect_equal() {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, &ChSettings::default()).await;
    run(
        &mut session,
        "CREATE DATABASE wrapped;
         CREATE DATABASE plain;
         CREATE TABLE wrapped.t (
           a UInt64, b String, d Date DEFAULT (today()), t Date TTL (d + INTERVAL 1 DAY),
           x UInt64 MATERIALIZED ((a * 2) + 1),
           INDEX i1 ((a)) TYPE minmax, INDEX i2 ((a), (b)) TYPE set(10),
           CONSTRAINT c1 CHECK (a > 0)
         ) ENGINE = MergeTree PARTITION BY ((toYYYYMM(d))) PRIMARY KEY ((a), (cityHash64(b)))
         ORDER BY ((a), (cityHash64(b)), (d)) SAMPLE BY (cityHash64(b))
         TTL (d + INTERVAL 1 YEAR) DELETE WHERE (a = 1);
         CREATE TABLE plain.t (
           a UInt64, b String, d Date DEFAULT today(), t Date TTL d + INTERVAL 1 DAY,
           x UInt64 MATERIALIZED a * 2 + 1,
           INDEX i1 a TYPE minmax, INDEX i2 (a, b) TYPE set(10),
           CONSTRAINT c1 CHECK a > 0
         ) ENGINE = MergeTree PARTITION BY toYYYYMM(d) PRIMARY KEY (a, cityHash64(b))
         ORDER BY (a, cityHash64(b), d) SAMPLE BY cityHash64(b)
         TTL d + INTERVAL 1 YEAR WHERE a = 1;",
    )
    .await;
    let model = session.inspect(&all()).await.unwrap();
    let wrapped = &model.databases["wrapped"].tables["t"];
    assert_eq!(wrapped, &model.databases["plain"].tables["t"]);
    assert_eq!(wrapped.order_by, ["a", "cityHash64(b)", "d"]);
    assert_eq!(wrapped.indexes["i2"].expression, "(a, b)");
    assert_eq!(
        wrapped.ttl.as_deref(),
        Some("d + toIntervalYear(1) WHERE a = 1")
    );
}
