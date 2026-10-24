use openmig_testkit::{ChLease, PgLease, clickhouse, postgres};
use std::process::Command;

fn psql(lease: &PgLease, url: &str, sql: &str) -> String {
    let output = Command::new(lease.bin_dir().join("psql"))
        .args([
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-d",
            url,
            "-c",
            sql,
        ])
        .output()
        .expect("run psql");
    assert!(
        output.status.success(),
        "{sql}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn pool_size(variable: &str, default: usize) -> usize {
    std::env::var(variable).map_or(default, |v| v.parse().unwrap())
}

fn assert_send<T: Send>(_: &T) {}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_leases_are_exclusive_and_reset_on_reuse() {
    let first = postgres().await;
    assert_send(&first);
    let first_url = first.url().to_owned();

    if pool_size("OPENMIG_TEST_PG_SERVERS", 2) >= 2 {
        let second = tokio::spawn(postgres()).await.unwrap();
        assert_ne!(second.url(), first_url);
        drop(second);
    }

    let app_url = first.create_database("app").await;
    assert_eq!(app_url, first.database_url("app"));
    psql(&first, &app_url, "create table t (id int)");
    psql(&first, first.url(), "create role reader");
    psql(&first, first.url(), "create table in_postgres (id int)");
    psql(&first, &app_url, "grant select on t to reader");
    drop(first);

    let reused = postgres().await;
    assert_eq!(
        reused.url(),
        first_url,
        "the most recently returned server is reused"
    );
    assert_eq!(
        psql(
            &reused,
            reused.url(),
            "select string_agg(datname, ',' order by datname) from pg_database"
        ),
        "postgres,template0,template1"
    );
    assert_eq!(
        psql(
            &reused,
            reused.url(),
            "select count(*) from pg_roles where rolname !~ '^pg_' and rolname <> current_user"
        ),
        "0"
    );
    assert_eq!(
        psql(
            &reused,
            reused.url(),
            "select count(*) from pg_class where relname = 'in_postgres'"
        ),
        "0"
    );
}

async fn ch(lease: &ChLease, sql: &str) -> String {
    match lease.query(sql).await {
        Ok(body) => body.trim_end().to_owned(),
        Err(error) => panic!("{sql}\n{error}"),
    }
}

#[tokio::test]
async fn clickhouse_lease_is_reset_on_reuse() {
    let lease = clickhouse().await;
    let url = lease.url().to_owned();
    assert_eq!(lease.cluster(), "openmig");
    ch(
        &lease,
        "CREATE DATABASE d ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/d', '{shard}', '{replica}')",
    )
    .await;
    ch(
        &lease,
        "CREATE TABLE d.t (x UInt64) ENGINE = ReplicatedMergeTree ORDER BY x SETTINGS storage_policy = 'encrypted'",
    )
    .await;
    ch(
        &lease,
        "CREATE TABLE default.t (x UInt64) ENGINE = MergeTree ORDER BY x",
    )
    .await;
    ch(&lease, "CREATE VIEW default.v AS SELECT x FROM default.t").await;
    ch(
        &lease,
        "CREATE DICTIONARY default.dict (x UInt64) PRIMARY KEY x SOURCE(CLICKHOUSE(TABLE 't')) LAYOUT(FLAT()) LIFETIME(0)",
    )
    .await;
    ch(&lease, "CREATE TABLE default.ts ENGINE = TimeSeries").await;
    ch(&lease, "CREATE ROLE r").await;
    ch(&lease, "CREATE ROLE rr IN replicated").await;
    ch(
        &lease,
        "CREATE USER u IDENTIFIED WITH no_password DEFAULT ROLE r",
    )
    .await;
    ch(&lease, "CREATE SETTINGS PROFILE p SETTINGS max_threads = 1").await;
    ch(
        &lease,
        "CREATE QUOTA q FOR INTERVAL 1 hour MAX queries = 100 TO r",
    )
    .await;
    ch(
        &lease,
        "CREATE ROW POLICY pol ON default.t USING x > 1 TO r",
    )
    .await;
    ch(&lease, "CREATE FUNCTION plus_one AS (x) -> x + 1").await;
    ch(&lease, "CREATE NAMED COLLECTION nc AS key = 'value'").await;
    drop(lease);

    let lease = clickhouse().await;
    assert_eq!(lease.url(), url);
    assert_eq!(
        ch(
            &lease,
            "SELECT name FROM system.databases WHERE name NOT IN ('system', 'information_schema', 'INFORMATION_SCHEMA') ORDER BY name"
        )
        .await,
        "default"
    );
    assert_eq!(
        ch(
            &lease,
            "SELECT count() FROM system.tables WHERE database = 'default'"
        )
        .await,
        "0"
    );
    for table in [
        "users",
        "roles",
        "settings_profiles",
        "quotas",
        "row_policies",
    ] {
        assert_eq!(
            ch(
                &lease,
                &format!("SELECT count() FROM system.{table} WHERE storage != 'users_xml'")
            )
            .await,
            "0",
            "{table}"
        );
    }
    assert_eq!(
        ch(
            &lease,
            "SELECT count() FROM system.functions WHERE origin = 'SQLUserDefined'"
        )
        .await,
        "0"
    );
    assert_eq!(
        ch(&lease, "SELECT count() FROM system.named_collections").await,
        "0"
    );
    ch(
        &lease,
        "CREATE DATABASE d ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/d', '{shard}', '{replica}')",
    )
    .await;
}
