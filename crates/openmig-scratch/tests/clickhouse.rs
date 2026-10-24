//! ClickHouse servers are heavy: every test holds `SERIAL` so only one server runs at a time.

use openmig_scratch::{ChSpec, ClickHouseServer, ScratchError, ScratchServer, Spec};
use std::collections::BTreeMap;
use std::time::Instant;
use tokio::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::const_new(());

async fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().await
}

fn spec() -> ChSpec {
    ChSpec {
        cluster: Some("openmig".to_owned()),
        storage_policies: vec!["encrypted".to_owned()],
        profile_settings: BTreeMap::from([(
            "allow_experimental_time_series_table".to_owned(),
            "1".to_owned(),
        )]),
        ..ChSpec::default()
    }
}

async fn start() -> ClickHouseServer {
    let started = Instant::now();
    let server = ClickHouseServer::start(&spec()).await.unwrap();
    eprintln!(
        "clickhouse {} started in {} ms",
        server.version(),
        started.elapsed().as_millis()
    );
    server
}

async fn exec(server: &ClickHouseServer, sql: &str) -> String {
    match server.query(sql).await {
        Ok(body) => body.trim_end().to_owned(),
        Err(error) => panic!("{sql}\n{error}"),
    }
}

fn outcome(result: &Result<String, ScratchError>) -> String {
    match result {
        Ok(body) if body.trim().is_empty() => "ok".to_owned(),
        Ok(body) => format!("ok: {}", body.trim_end()),
        Err(ScratchError::Query { message, .. }) => {
            format!("error: {}", message.lines().next().unwrap_or_default())
        }
        Err(error) => format!("error: {error}"),
    }
}

#[tokio::test]
async fn replicated_engines_and_cluster_ddl() {
    let _serial = serial().await;
    let server = start().await;
    assert!(server.url().starts_with("http://default@127.0.0.1:"));
    assert_eq!(server.cluster(), "openmig");
    assert_eq!(
        exec(
            &server,
            "SELECT getMacro('cluster'), getMacro('shard'), getMacro('replica')"
        )
        .await,
        "openmig\t01\tr1"
    );
    assert_eq!(
        exec(
            &server,
            "SELECT host_address, port FROM system.clusters WHERE cluster = 'openmig'"
        )
        .await,
        format!("127.0.0.1\t{}", server.tcp_port())
    );

    exec(
        &server,
        "CREATE DATABASE d ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/d', '{shard}', '{replica}')",
    )
    .await;
    exec(
        &server,
        "CREATE TABLE d.t (x UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}') ORDER BY x",
    )
    .await;
    exec(&server, "INSERT INTO d.t VALUES (1), (2)").await;
    assert_eq!(exec(&server, "SELECT sum(x) FROM d.t").await, "3");
    assert_eq!(
        exec(
            &server,
            "SELECT engine FROM system.databases WHERE name = 'd'"
        )
        .await,
        "Replicated"
    );

    exec(
        &server,
        "CREATE DATABASE a ON CLUSTER '{cluster}' ENGINE = Atomic",
    )
    .await;
    exec(
        &server,
        "CREATE TABLE a.t ON CLUSTER '{cluster}' (x UInt64) ENGINE = MergeTree ORDER BY x",
    )
    .await;
    exec(
        &server,
        "CREATE TABLE a.encrypted (x UInt64) ENGINE = MergeTree ORDER BY x SETTINGS storage_policy = 'encrypted'",
    )
    .await;
    assert_eq!(
        exec(
            &server,
            "SELECT storage_policy FROM system.tables WHERE database = 'a' AND name = 'encrypted'"
        )
        .await,
        "encrypted"
    );
    let missing = server
        .query("CREATE TABLE a.bad (x UInt64) ENGINE = MergeTree ORDER BY x SETTINGS storage_policy = 'missing'")
        .await;
    assert!(missing.is_err());

    exec(&server, "CREATE ROLE r ON CLUSTER '{cluster}'").await;
    exec(
        &server,
        "GRANT ON CLUSTER '{cluster}' ALL ON *.* TO r WITH GRANT OPTION",
    )
    .await;
    assert_eq!(
        exec(&server, "SELECT storage FROM system.roles WHERE name = 'r'").await,
        "local_directory"
    );

    exec(
        &server,
        "CREATE TABLE a.lock (k String, v String) ENGINE = KeeperMap('/openmig/lock') PRIMARY KEY k",
    )
    .await;
    exec(
        &server,
        "INSERT INTO a.lock SETTINGS keeper_map_strict_mode = 1 VALUES ('lock', 'one')",
    )
    .await;
    let second = server
        .query("INSERT INTO a.lock SETTINGS keeper_map_strict_mode = 1 VALUES ('lock', 'two')")
        .await;
    assert!(
        matches!(&second, Err(ScratchError::Query { message, .. }) if message.contains("KEEPER_EXCEPTION") || message.contains("exists")),
        "{second:?}"
    );
    assert_eq!(exec(&server, "SELECT v FROM a.lock").await, "one");
    exec(&server, "ALTER TABLE a.lock DELETE WHERE k = 'lock'").await;
    exec(
        &server,
        "INSERT INTO a.lock SETTINGS keeper_map_strict_mode = 1 VALUES ('lock', 'three')",
    )
    .await;

    server.stop().await.unwrap();
}

#[tokio::test]
async fn access_storages() {
    let _serial = serial().await;
    let server = start().await;
    assert_eq!(
        exec(
            &server,
            "SELECT name FROM system.user_directories ORDER BY precedence"
        )
        .await,
        "users_xml\nlocal_directory\nreplicated"
    );
    exec(&server, "CREATE ROLE local_role IN local_directory").await;
    exec(&server, "CREATE ROLE replicated_role IN replicated").await;
    exec(&server, "CREATE ROLE default_role").await;
    exec(
        &server,
        "CREATE USER u IDENTIFIED WITH no_password DEFAULT ROLE default_role",
    )
    .await;
    exec(&server, "GRANT SELECT ON system.* TO local_role").await;
    assert_eq!(
        exec(
            &server,
            "SELECT name, storage FROM system.roles ORDER BY name"
        )
        .await,
        "default_role\tlocal_directory\nlocal_role\tlocal_directory\nreplicated_role\treplicated"
    );
    assert!(server.dir().join("access").join("roles.list").exists());
    exec(&server, "CREATE NAMED COLLECTION nc AS key = 'value'").await;
    exec(&server, "CREATE FUNCTION plus_one AS (x) -> x + 1").await;
    assert_eq!(exec(&server, "SELECT plus_one(1)").await, "2");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn findings() {
    let _serial = serial().await;
    let server = start().await;
    let mut findings = Vec::new();

    let create_d = "CREATE DATABASE d ON CLUSTER '{cluster}' ENGINE = Replicated('/clickhouse/databases/d', '{shard}', '{replica}')";
    exec(&server, create_d).await;
    exec(
        &server,
        "CREATE TABLE d.t (x UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}') ORDER BY x",
    )
    .await;
    exec(
        &server,
        "CREATE TABLE d.implicit (x UInt64) ENGINE = ReplicatedMergeTree ORDER BY x",
    )
    .await;
    let engine_full = exec(
        &server,
        "SELECT name, engine_full FROM system.tables WHERE database = 'd' ORDER BY name FORMAT TSVRaw",
    )
    .await;
    let create_query = exec(
        &server,
        "SELECT name, create_table_query FROM system.tables WHERE database = 'd' ORDER BY name FORMAT TSVRaw",
    )
    .await;
    let show_create = exec(&server, "SHOW CREATE TABLE d.t FORMAT TSVRaw").await;
    findings.push(format!(
        "(b) system.tables.engine_full:\n{engine_full}\n    create_table_query:\n{create_query}\n    SHOW CREATE TABLE d.t:\n{show_create}"
    ));

    exec(&server, "DROP DATABASE d SYNC").await;
    let recreate = server.query(create_d).await;
    findings.push(format!(
        "(a) DROP DATABASE d SYNC then CREATE DATABASE d with the same Replicated path: {}",
        outcome(&recreate)
    ));
    if recreate.is_ok() {
        let table = server
            .query("CREATE TABLE d.t (x UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}') ORDER BY x")
            .await;
        findings.push(format!(
            "(a) re-creating d.t in the re-created database: {}",
            outcome(&table)
        ));
    }

    for database in ["a", "d"] {
        if database == "a" {
            exec(&server, "CREATE DATABASE a ENGINE = Atomic").await;
        }
        exec(
            &server,
            &format!("CREATE TABLE {database}.src (x UInt64) ENGINE = MergeTree ORDER BY x"),
        )
        .await;
        exec(
            &server,
            &format!("CREATE TABLE {database}.dst (x UInt64) ENGINE = MergeTree ORDER BY x"),
        )
        .await;
        exec(
            &server,
            &format!("CREATE MATERIALIZED VIEW {database}.mv TO {database}.dst AS SELECT x FROM {database}.src"),
        )
        .await;
        let modify = server
            .query(&format!(
                "ALTER TABLE {database}.mv MODIFY QUERY SELECT x * 2 AS x FROM {database}.src"
            ))
            .await;
        let after = server
            .query(&format!(
                "SELECT as_select FROM system.tables WHERE database = '{database}' AND name = 'mv'"
            ))
            .await;
        findings.push(format!(
            "(c) ALTER TABLE ... MODIFY QUERY on a materialized view with TO ({} database): {}; as_select now: {}",
            if database == "a" { "Atomic" } else { "Replicated" },
            outcome(&modify),
            outcome(&after)
        ));
    }

    exec(&server, "CREATE ROLE grant_role").await;
    exec(&server, "GRANT ALL ON a.* TO grant_role").await;
    let grants = server.query("SHOW GRANTS FOR grant_role").await;
    findings.push(format!(
        "(d) SHOW GRANTS FOR grant_role after GRANT ALL ON a.*: {}",
        outcome(&grants)
    ));
    exec(&server, "CREATE ROLE global_role").await;
    exec(&server, "GRANT ALL ON *.* TO global_role").await;
    let grants = server.query("SHOW GRANTS FOR global_role").await;
    findings.push(format!(
        "(d) SHOW GRANTS FOR global_role after GRANT ALL ON *.*: {}",
        outcome(&grants)
    ));
    exec(&server, "CREATE ROLE select_role").await;
    exec(&server, "GRANT SELECT, INSERT ON a.* TO select_role").await;
    let grants = server.query("SHOW GRANTS FOR select_role").await;
    findings.push(format!(
        "(d) SHOW GRANTS FOR select_role after GRANT SELECT, INSERT ON a.*: {}",
        outcome(&grants)
    ));

    let setting = server
        .query(
            "SELECT value FROM system.settings WHERE name = 'allow_experimental_time_series_table'",
        )
        .await;
    let time_series = server.query("CREATE TABLE a.ts ENGINE = TimeSeries").await;
    let ts_create = server.query("SHOW CREATE TABLE a.ts FORMAT TSVRaw").await;
    findings.push(format!(
        "(e) allow_experimental_time_series_table = {}; CREATE TABLE a.ts ENGINE = TimeSeries: {}\n    {}",
        outcome(&setting),
        outcome(&time_series),
        outcome(&ts_create)
    ));

    println!("\n==== ClickHouse {} findings ====", server.version());
    for finding in &findings {
        println!("{finding}");
    }
    println!("==== end of findings ====\n");

    assert!(time_series.is_ok(), "{time_series:?}");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn lifecycle_and_extra_config() {
    let _serial = serial().await;
    let extra = tempfile::tempdir().unwrap();
    let extra_file = extra.path().join("custom.xml");
    std::fs::write(
        &extra_file,
        "<clickhouse><macros><custom>value</custom></macros></clickhouse>",
    )
    .unwrap();
    let spec = ChSpec {
        extra_config: Some(extra_file),
        macros: BTreeMap::from([("replica".to_owned(), "replica-x".to_owned())]),
        ..ChSpec::default()
    };
    let server = ScratchServer::start(&Spec::ClickHouse(spec)).await.unwrap();
    let ScratchServer::ClickHouse(ch) = &server else {
        unreachable!()
    };
    assert_eq!(ch.cluster(), "openmig");
    assert_eq!(
        exec(
            ch,
            "SELECT getMacro('custom'), getMacro('replica'), getMacro('cluster')"
        )
        .await,
        "value\treplica-x\topenmig"
    );
    let dir = ch.dir().to_path_buf();
    let pid = ch.pid();
    assert!(dir.join("config.d").join("extra.xml").exists());
    let stopping = Instant::now();
    server.stop().await.unwrap();
    eprintln!(
        "clickhouse stopped in {} ms",
        stopping.elapsed().as_millis()
    );
    assert!(!dir.exists());
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());

    let server = start().await;
    let dir = server.dir().to_path_buf();
    drop(server);
    assert!(!dir.exists());
}

#[tokio::test]
async fn failed_start_reports_the_log() {
    let _serial = serial().await;
    let extra = tempfile::tempdir().unwrap();
    let broken = extra.path().join("broken.xml");
    std::fs::write(
        &broken,
        "<clickhouse><http_port>not-a-port</http_port></clickhouse>",
    )
    .unwrap();
    let started = Instant::now();
    let error = ClickHouseServer::start(&ChSpec {
        extra_config: Some(broken),
        ..ChSpec::default()
    })
    .await
    .err()
    .unwrap();
    eprintln!("failed after {} ms: {error}", started.elapsed().as_millis());
    let ScratchError::StartFailed { log, .. } = &error else {
        panic!("unexpected error: {error}");
    };
    assert!(log.contains("not-a-port"), "{log}");

    let error = ClickHouseServer::start(&ChSpec {
        version: Some(1),
        ..ChSpec::default()
    })
    .await
    .err()
    .unwrap();
    assert!(matches!(
        error,
        ScratchError::VersionMismatch { expected: 1, .. }
    ));
}

#[tokio::test]
async fn unknown_profile_setting_fails_fast() {
    let _serial = serial().await;
    let error = ClickHouseServer::start(&ChSpec {
        profile_settings: BTreeMap::from([("no_such_setting".to_owned(), "1".to_owned())]),
        ..ChSpec::default()
    })
    .await
    .err()
    .unwrap();
    let ScratchError::StartFailed { log, .. } = &error else {
        panic!("unexpected error: {error}");
    };
    assert!(log.contains("Unknown setting 'no_such_setting'"), "{log}");
}

#[tokio::test]
async fn runs_as_configured_user_when_root() {
    if !nix::unistd::geteuid().is_root() {
        return;
    }
    let _serial = serial().await;
    let server = ClickHouseServer::start(&ChSpec {
        run_as: Some("nobody".to_owned()),
        ..spec()
    })
    .await
    .unwrap();
    let nobody = nix::unistd::User::from_name("nobody").unwrap().unwrap();
    let status = std::fs::read_to_string(format!("/proc/{}/status", server.pid())).unwrap();
    let uid_line = status.lines().find(|l| l.starts_with("Uid:")).unwrap();
    assert!(
        uid_line
            .split_whitespace()
            .skip(1)
            .all(|uid| uid == nobody.uid.to_string()),
        "{uid_line}"
    );
    exec(
        &server,
        "CREATE TABLE t ON CLUSTER '{cluster}' (x UInt64) ENGINE = ReplicatedMergeTree ORDER BY x",
    )
    .await;
    exec(&server, "CREATE ROLE r").await;
    server.stop().await.unwrap();
}
