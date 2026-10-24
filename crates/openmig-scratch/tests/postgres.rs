use openmig_scratch::{PgSpec, PostgresServer, ScratchError, ScratchServer, Spec};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Environment variable that turns `helper` into a child-process entry point for tests that need a
/// separate OpenMig process.
const HELPER: &str = "OPENMIG_SCRATCH_TEST_HELPER";

fn psql_binary(server: &PostgresServer) -> PathBuf {
    let next_to_server = server.bin_dir().join("psql");
    if next_to_server.is_file() {
        next_to_server
    } else {
        PathBuf::from("psql")
    }
}

fn psql(server: &PostgresServer, url: &str, sql: &str) -> Result<String, String> {
    let output = Command::new(psql_binary(server))
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
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn database_url(server: &PostgresServer, database: &str) -> String {
    server
        .url()
        .replacen("/postgres?", &format!("/{database}?"), 1)
}

fn is_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesised command name; zombies are dead for our purposes.
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

async fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !is_alive(pid)
}

/// A child copy of this test binary running `helper`; killed when dropped.
struct Helper {
    child: std::process::Child,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
}

impl Helper {
    fn start(mode: &str, envs: &[(&str, &Path)]) -> Self {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["helper", "--exact", "--nocapture", "--test-threads=1"])
            .env(HELPER, mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn helper");
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Helper { child, lines }
    }

    /// Values the helper reports as `@@key=value` until it reports `@@<done>`. libtest output
    /// shares the lines, hence the marker.
    fn read_until(&mut self, done: &str) -> Vec<(String, String)> {
        let mut values = Vec::new();
        for line in self.lines.by_ref() {
            let line = line.unwrap();
            let Some((_, report)) = line.split_once("@@") else {
                continue;
            };
            if report == done {
                return values;
            }
            if let Some((key, value)) = report.split_once('=') {
                values.push((key.to_owned(), value.to_owned()));
            }
        }
        panic!("helper exited before reporting {done}: {values:?}");
    }

    fn wait_success(mut self) -> bool {
        for line in self.lines.by_ref() {
            line.unwrap();
        }
        self.child.wait().unwrap().success()
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.kill();
    }
}

fn value<'a>(values: &'a [(String, String)], key: &str) -> &'a str {
    values
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("helper did not report {key}"))
}

/// Entry point for child processes started by other tests; a no-op otherwise.
#[tokio::test]
async fn helper() {
    let Ok(mode) = std::env::var(HELPER) else {
        return;
    };
    match mode.as_str() {
        "orphan" => {
            let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
            println!("@@pid={}", server.pid());
            println!("@@dir={}", server.socket_dir().display());
            println!("@@ready");
            // Keep the server referenced until the parent kills this process.
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let _ = server.url();
            }
        }
        "timings" => {
            for round in ["first", "second"] {
                let started = Instant::now();
                let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
                println!("@@{round}_ms={}", started.elapsed().as_millis());
                psql(&server, server.url(), "select 1").unwrap();
                server.stop().await.unwrap();
            }
            println!("@@done");
        }
        other => panic!("unknown helper mode {other}"),
    }
}

#[tokio::test]
async fn serves_queries_as_superuser() {
    let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
    assert!(server.version().starts_with(|c: char| c.is_ascii_digit()));
    assert!(server.url().starts_with("postgresql:///postgres?host=/"));
    assert_eq!(server.port(), 5432);
    assert_eq!(server.user(), "openmig");
    assert!(server.socket_dir().join(".s.PGSQL.5432").exists());
    assert!(server.socket_dir().as_os_str().len() < 64);

    let url = server.url();
    assert_eq!(psql(&server, url, "select 1").unwrap(), "1");
    assert_eq!(
        psql(
            &server,
            url,
            "select current_user, usesuper from pg_user where usename = current_user"
        )
        .unwrap(),
        "openmig|t"
    );
    assert_eq!(
        psql(
            &server,
            url,
            "select pg_encoding_to_char(encoding), datcollate, datctype from pg_database where datname = 'postgres'"
        )
        .unwrap(),
        "UTF8|C.UTF-8|C.UTF-8"
    );
    let settings = psql(
        &server,
        url,
        "select current_setting('fsync'), current_setting('listen_addresses'), current_setting('max_connections')",
    )
    .unwrap();
    assert_eq!(settings, "off||100");

    psql(&server, url, "create role app login").unwrap();
    psql(&server, url, "create database scratch_db owner app").unwrap();
    let db_url = database_url(&server, "scratch_db");
    psql(&server, &db_url, "create table t (id int primary key)").unwrap();

    let mut holder = Command::new(psql_binary(&server))
        .args(["-X", "-q", "-d", &db_url, "-c", "select pg_sleep(60)"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while psql(
        &server,
        url,
        "select count(*) from pg_stat_activity where datname = 'scratch_db'",
    )
    .unwrap()
        == "0"
    {
        assert!(
            Instant::now() < deadline,
            "the holding session never connected"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    psql(&server, url, "drop database scratch_db with (force)").unwrap();
    assert!(!holder.wait().unwrap().success());
    psql(&server, url, "drop role app").unwrap();

    server.stop().await.unwrap();
}

#[tokio::test]
async fn runs_as_unprivileged_user_when_root() {
    if !nix::unistd::geteuid().is_root() {
        return;
    }
    let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
    let nobody = nix::unistd::User::from_name("nobody").unwrap().unwrap();
    let owner = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(server.data_dir()).unwrap());
    assert_eq!(owner, nobody.uid.as_raw());
    let status = std::fs::read_to_string(format!("/proc/{}/status", server.pid())).unwrap();
    let uid_line = status.lines().find(|l| l.starts_with("Uid:")).unwrap();
    assert!(
        uid_line
            .split_whitespace()
            .skip(1)
            .all(|uid| uid == nobody.uid.to_string())
    );

    let error = PostgresServer::start(&PgSpec {
        run_as: Some("openmig-no-such-user".to_owned()),
        ..PgSpec::default()
    })
    .await
    .err()
    .unwrap();
    assert!(matches!(error, ScratchError::UnknownUser(name) if name == "openmig-no-such-user"));
    server.stop().await.unwrap();
}

#[tokio::test]
async fn stop_removes_the_directory() {
    let server = ScratchServer::start(&Spec::Postgres(PgSpec::default()))
        .await
        .unwrap();
    let ScratchServer::Postgres(pg) = &server else {
        unreachable!()
    };
    let dir = pg.socket_dir().to_path_buf();
    let pid = pg.pid();
    assert!(dir.exists());
    server.stop().await.unwrap();
    assert!(!dir.exists());
    assert!(!is_alive(pid));
}

#[tokio::test]
async fn drop_kills_the_server() {
    let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
    let dir = server.socket_dir().to_path_buf();
    let pid = server.pid();
    drop(server);
    assert!(!dir.exists());
    assert!(wait_until_dead(pid, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn servers_run_concurrently() {
    let spec = PgSpec::default();
    let (a, b) = tokio::join!(PostgresServer::start(&spec), PostgresServer::start(&spec));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.socket_dir(), b.socket_dir());
    psql(&a, a.url(), "create table only_in_a (id int)").unwrap();
    assert_eq!(
        psql(
            &b,
            b.url(),
            "select count(*) from pg_class where relname = 'only_in_a'"
        )
        .unwrap(),
        "0"
    );
    a.stop().await.unwrap();
    assert_eq!(psql(&b, b.url(), "select 1").unwrap(), "1");
    b.stop().await.unwrap();
}

#[tokio::test]
async fn server_survives_the_thread_that_started_it() {
    let server = std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(PostgresServer::start(&PgSpec::default()))
            .unwrap()
    })
    .join()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(is_alive(server.pid()));
    assert_eq!(psql(&server, server.url(), "select 1").unwrap(), "1");
    server.stop().await.unwrap();
}

#[tokio::test]
async fn parent_death_kills_the_server() {
    let mut helper = Helper::start("orphan", &[]);
    let values = helper.read_until("ready");
    let pid: u32 = value(&values, "pid").parse().unwrap();
    let dir = PathBuf::from(value(&values, "dir"));
    assert!(is_alive(pid));

    helper.kill();
    assert!(
        wait_until_dead(pid, Duration::from_secs(10)).await,
        "postgres {pid} outlived its parent"
    );

    let server = PostgresServer::start(&PgSpec::default()).await.unwrap();
    assert!(
        !dir.exists(),
        "stale directory {} was not removed",
        dir.display()
    );
    server.stop().await.unwrap();
}

#[tokio::test]
async fn cached_data_directory_starts_faster() {
    let cache = tempfile::tempdir().unwrap();
    let mut helper = Helper::start("timings", &[("XDG_CACHE_HOME", cache.path())]);
    let values = helper.read_until("done");
    assert!(helper.wait_success());
    let first: u64 = value(&values, "first_ms").parse().unwrap();
    let second: u64 = value(&values, "second_ms").parse().unwrap();
    eprintln!("postgres start: cold (initdb) {first} ms, cached {second} ms");
    let entries: Vec<_> = std::fs::read_dir(cache.path().join("openmig").join("pg"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(entries[0].join("data").join("PG_VERSION").is_file());
    assert!(
        second < first,
        "cached start ({second} ms) not faster than initdb ({first} ms)"
    );
}

#[tokio::test]
async fn unwritable_cache_falls_back_to_initdb() {
    let tmp = tempfile::tempdir().unwrap();
    let not_a_dir = tmp.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let mut helper = Helper::start("timings", &[("XDG_CACHE_HOME", &not_a_dir)]);
    helper.read_until("done");
    assert!(helper.wait_success());
}

#[tokio::test]
async fn reports_missing_binaries_and_version_mismatch() {
    let empty = tempfile::tempdir().unwrap();
    let error = PostgresServer::start(&PgSpec {
        bin_dir: Some(empty.path().to_path_buf()),
        ..PgSpec::default()
    })
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&error, ScratchError::BinaryNotFound { binary, .. } if binary == "initdb"),
        "{error}"
    );

    let error = PostgresServer::start(&PgSpec {
        version: Some(1),
        ..PgSpec::default()
    })
    .await
    .err()
    .unwrap();
    assert!(
        matches!(&error, ScratchError::VersionMismatch { expected: 1, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn failed_start_reports_the_log() {
    let error = PostgresServer::start(&PgSpec {
        settings: [("no_such_setting".to_owned(), "1".to_owned())].into(),
        ..PgSpec::default()
    })
    .await
    .err()
    .unwrap();
    let ScratchError::StartFailed { log, .. } = &error else {
        panic!("unexpected error: {error}");
    };
    assert!(log.contains("no_such_setting"), "{log}");
}
