//! Convergence on a real server: states A and B built with SQL, then A is migrated to B with
//! the planned statements (and back), and must inspect as the target state.

mod common;

use common::{connect, run};
use openmig_clickhouse::{ChSession, ChSettings, ClickHouseEngine, Model};
use openmig_core::{DiffContext, Engine, PlanMode, RenameSet, Scope, Session};

fn scope() -> Scope {
    Scope {
        manage_roles: true,
        ..Scope::default()
    }
}

fn plan_sql(settings: &ChSettings, current: &Model, desired: &Model) -> Vec<String> {
    let scope = scope();
    let renames = RenameSet::default();
    let ctx = DiffContext::<ClickHouseEngine> {
        settings,
        scope: &scope,
        renames: &renames,
        mode: PlanMode::Sync,
        allow_recreate: false,
    };
    let changes =
        ClickHouseEngine::diff(current, desired, &ctx).unwrap_or_else(|e| panic!("diff: {e}"));
    let plan = ClickHouseEngine::plan(changes, current, desired, &ctx)
        .unwrap_or_else(|e| panic!("plan: {e}"));
    plan.statements.into_iter().map(|s| s.sql).collect()
}

async fn state(session: &mut ChSession, sql: &str) -> Model {
    session.reset().await.unwrap();
    run(session, sql).await;
    session.inspect(&scope()).await.unwrap()
}

async fn migrate(session: &mut ChSession, settings: &ChSettings, to: &Model, label: &str) {
    let current = session.inspect(&scope()).await.unwrap();
    let statements = plan_sql(settings, &current, to);
    assert!(!statements.is_empty(), "{label}: nothing to do");
    for sql in &statements {
        if let Err(e) = session.execute(sql).await {
            panic!("{label}: {sql}\n{e}\nplan:\n{}", statements.join(";\n"));
        }
    }
    let after = session.inspect(&scope()).await.unwrap();
    let remaining = plan_sql(settings, &after, to);
    assert!(
        remaining.is_empty(),
        "{label}: does not converge; remaining:\n{}\napplied:\n{}",
        remaining.join(";\n"),
        statements.join(";\n")
    );
}

async fn converge(settings: ChSettings, a: &str, b: &str) {
    let lease = openmig_testkit::clickhouse().await;
    let mut session = connect(&lease, &settings).await;
    let model_a = state(&mut session, a).await;
    let model_b = state(&mut session, b).await;
    assert_ne!(model_a, model_b);
    state(&mut session, a).await;
    migrate(&mut session, &settings, &model_b, "A -> B").await;
    migrate(&mut session, &settings, &model_a, "B -> A").await;
}

const TABLES_A: &str = "
CREATE DATABASE app COMMENT 'v1';
CREATE TABLE app.events (
  ts DateTime64(3) CODEC(Delta(8), ZSTD(1)),
  kind String DEFAULT 'none',
  user_id UInt64 MATERIALIZED 0,
  payload String COMMENT 'json' CODEC(ZSTD(1)),
  note Nullable(String) TTL toDateTime(ts) + INTERVAL 1 DAY,
  legacy UInt8,
  INDEX idx_kind kind TYPE set(100) GRANULARITY 2,
  INDEX idx_payload payload TYPE tokenbf_v1(32768, 3, 0),
  PROJECTION by_kind (SELECT kind, count() GROUP BY kind),
  CONSTRAINT kind_known CHECK kind != ''
) ENGINE = MergeTree PARTITION BY toYYYYMM(ts) ORDER BY (ts, kind, cityHash64(kind))
TTL toDateTime(ts) + INTERVAL 30 DAY
SETTINGS ttl_only_drop_parts = 1, merge_with_ttl_timeout = 3600
COMMENT 'events';
CREATE TABLE app.counts (kind String, n UInt64) ENGINE = SummingMergeTree ORDER BY kind;
CREATE MATERIALIZED VIEW app.counts_mv TO app.counts AS SELECT kind, count() AS n FROM app.events GROUP BY kind;
CREATE VIEW app.recent AS SELECT ts, kind FROM app.events WHERE ts > now() - INTERVAL 1 DAY;
CREATE FUNCTION app_double AS x -> x * 2;
CREATE TABLE app.countries (id UInt64, name String) ENGINE = MergeTree ORDER BY id;
CREATE DICTIONARY app.country_names (id UInt64, name String DEFAULT '?')
  PRIMARY KEY id SOURCE(CLICKHOUSE(TABLE 'countries' DB 'app')) LIFETIME(300) LAYOUT(HASHED());
CREATE ROLE reader;
CREATE ROLE writer;
CREATE ROLE admin;
GRANT SELECT ON app.* TO reader;
GRANT INSERT, SELECT ON app.events TO writer WITH GRANT OPTION;
GRANT ALL ON app.* TO admin;
GRANT reader TO writer;
CREATE ROW POLICY tenant ON app.events FOR SELECT USING kind != 'secret' TO reader;
";

const TABLES_B: &str = "
CREATE DATABASE app COMMENT 'v2';
CREATE TABLE app.events (
  ts DateTime64(3) CODEC(Delta(8), ZSTD(3)),
  kind String DEFAULT 'none',
  user_id UInt64 DEFAULT 42 COMMENT 'user',
  payload LowCardinality(String),
  note Nullable(String),
  source String DEFAULT 'web',
  INDEX idx_kind kind TYPE set(1000) GRANULARITY 2,
  INDEX idx_user user_id TYPE minmax,
  PROJECTION by_user (SELECT user_id, count() GROUP BY user_id),
  CONSTRAINT kind_known ASSUME kind != ''
) ENGINE = MergeTree PARTITION BY toYYYYMM(ts) ORDER BY (ts, kind, cityHash64(kind))
SAMPLE BY cityHash64(kind)
TTL toDateTime(ts) + INTERVAL 90 DAY
SETTINGS ttl_only_drop_parts = 0
COMMENT 'all events';
CREATE TABLE app.counts (kind String, n UInt64) ENGINE = SummingMergeTree ORDER BY kind;
CREATE MATERIALIZED VIEW app.counts_mv TO app.counts AS SELECT kind, toUInt64(count() * 2) AS n FROM app.events GROUP BY kind;
CREATE VIEW app.recent COMMENT 'last day' AS SELECT ts, kind, user_id FROM app.events WHERE ts > now() - INTERVAL 1 DAY;
CREATE VIEW app.by_source AS SELECT source, count() AS c FROM app.events GROUP BY source;
CREATE FUNCTION app_double AS x -> x * 3;
CREATE FUNCTION app_triple AS x -> x * 3;
CREATE TABLE app.countries (id UInt64, name String) ENGINE = MergeTree ORDER BY id;
CREATE DICTIONARY app.country_names (id UInt64, name String DEFAULT 'unknown')
  PRIMARY KEY id SOURCE(CLICKHOUSE(TABLE 'countries' DB 'app')) LIFETIME(600) LAYOUT(FLAT())
  COMMENT 'names';
CREATE ROLE reader;
CREATE ROLE writer;
CREATE ROLE auditor IN replicated;
GRANT SELECT, dictGet ON app.* TO reader;
GRANT INSERT, SELECT ON app.events TO writer;
GRANT SELECT(ts, kind) ON app.events TO auditor;
GRANT reader TO writer WITH ADMIN OPTION;
GRANT reader TO auditor;
CREATE ROW POLICY tenant ON app.events AS RESTRICTIVE FOR SELECT USING kind = 'public' TO ALL EXCEPT writer;
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tables_views_and_access() {
    converge(ChSettings::default(), TABLES_A, TABLES_B).await;
}

const CLUSTER_A: &str = "
CREATE DATABASE repl ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/repl', '{shard}', '{replica}');
CREATE DATABASE plain ON CLUSTER '{cluster}';
CREATE TABLE repl.events (id UInt64, kind String, ts DateTime) ENGINE = ReplicatedMergeTree ORDER BY id;
CREATE TABLE repl.metrics ENGINE = TimeSeries;
CREATE TABLE plain.events ON CLUSTER '{cluster}' (id UInt64, kind String) ENGINE = ReplicatedReplacingMergeTree ORDER BY id;
CREATE TABLE plain.agg ON CLUSTER '{cluster}' (kind String, n UInt64) ENGINE = MergeTree ORDER BY kind;
CREATE MATERIALIZED VIEW plain.agg_mv ON CLUSTER '{cluster}' TO plain.agg AS SELECT kind, count() AS n FROM plain.events GROUP BY kind;
CREATE ROLE gateway ON CLUSTER '{cluster}';
GRANT ON CLUSTER '{cluster}' SELECT ON repl.* TO gateway;
";

const CLUSTER_B: &str = "
CREATE DATABASE repl ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/repl', '{shard}', '{replica}') COMMENT 'replicated';
CREATE DATABASE plain ON CLUSTER '{cluster}';
CREATE TABLE repl.events (id UInt64, kind LowCardinality(String), ts DateTime, extra String DEFAULT '') ENGINE = ReplicatedMergeTree ORDER BY id TTL ts + INTERVAL 7 DAY;
CREATE TABLE repl.metrics ENGINE = TimeSeries;
CREATE TABLE plain.events ON CLUSTER '{cluster}' (id UInt64, kind String, extra UInt8) ENGINE = ReplicatedReplacingMergeTree ORDER BY id SETTINGS min_bytes_for_wide_part = 0;
CREATE TABLE plain.agg ON CLUSTER '{cluster}' (kind String, n UInt64) ENGINE = MergeTree ORDER BY kind;
CREATE MATERIALIZED VIEW plain.agg_mv ON CLUSTER '{cluster}' TO plain.agg AS SELECT kind, count() AS n FROM plain.events WHERE extra > 0 GROUP BY kind;
CREATE ROLE gateway ON CLUSTER '{cluster}';
CREATE ROLE shared IN replicated;
GRANT ON CLUSTER '{cluster}' SELECT, INSERT ON repl.* TO gateway;
GRANT SELECT ON plain.* TO shared;
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_and_replicated_databases() {
    let settings = ChSettings {
        cluster: Some("{cluster}".into()),
        ..ChSettings::default()
    };
    converge(settings, CLUSTER_A, CLUSTER_B).await;
}
