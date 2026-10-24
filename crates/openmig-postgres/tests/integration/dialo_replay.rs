//! Replay of a real migration history, checking a plan for every migration.
//!
//! `cargo test -p openmig-postgres --test integration -- --ignored --nocapture dialo_replay`
//!
//! Migrations come from `OPENMIG_DIALO_MIGRATIONS` (default: dialo's PostgreSQL migrations next
//! to the workspace). Two private servers are started so that roles are not shared: database B
//! replays the files one at a time; after each file the plan from database A (maintained only by
//! plans) to B is generated, applied to A, and A must then plan empty against B and equal it.
//! When a step fails, A is rebuilt by replaying the files so far and the replay goes on; the
//! failures are reported at the end. `OPENMIG_DIALO_FROM=<n>` starts checking at file `n`
//! (earlier files are replayed into both databases).

use crate::plan::{apply, comparable, diff_and_plan};
use openmig_core::sql::{Dialect, split};
use openmig_core::{PlanMode, RenameSet, Scope, Session, SessionPurpose};
use openmig_postgres::{PgSession, PgSettings};
use openmig_scratch::PostgresServer;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn migrations_dir() -> PathBuf {
    std::env::var_os("OPENMIG_DIALO_MIGRATIONS").map_or_else(
        || PathBuf::from("/home/coder/dialo/dialo-db/pg/migrations"),
        PathBuf::from,
    )
}

fn url(server: &PostgresServer, database: &str) -> String {
    crate::common::url_for(server.url(), database)
}

async fn session(url: &str) -> PgSession {
    PgSession::connect(url, &PgSettings::default(), SessionPurpose::Target)
        .await
        .unwrap_or_else(|e| panic!("connect {url}: {e}"))
}

/// Run a migration file: in one transaction unless it opts out or builds indexes concurrently.
async fn run_file(url: &str, path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let statements = split(&text, Dialect::Postgres).map_err(|e| e.to_string())?;
    let transactional = !text.contains("atlas:txmode none")
        && !statements.iter().any(|s| s.sql.contains("CONCURRENTLY"));
    let mut s = session(url).await;
    if transactional {
        s.begin().await.map_err(|e| e.to_string())?;
    }
    for st in &statements {
        s.execute(&st.sql)
            .await
            .map_err(|e| format!("{}: {e}\n{}", path.display(), st.sql))?;
    }
    if transactional {
        s.commit().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn recreate(server: &PostgresServer, database: &str) {
    let mut admin = session(server.url()).await;
    crate::common::run(
        &mut admin,
        &format!("DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)"),
    )
    .await;
    let roles: Vec<String> = admin
        .client()
        .query(
            "SELECT rolname::text FROM pg_roles WHERE oid <> 10 AND rolname !~ '^pg_' \
             AND rolname <> current_user ORDER BY 1",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    for r in roles {
        crate::common::run(
            &mut admin,
            &format!("DROP OWNED BY \"{r}\"; DROP ROLE \"{r}\""),
        )
        .await;
    }
    crate::common::run(
        &mut admin,
        &format!("CREATE DATABASE \"{database}\" TEMPLATE template0"),
    )
    .await;
}

#[tokio::test]
#[ignore = "replays dialo's migration history; slow"]
async fn dialo_replay() {
    let dir = migrations_dir();
    if !dir.is_dir() {
        eprintln!("skipping: {} is not a directory", dir.display());
        return;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    let from: usize = std::env::var("OPENMIG_DIALO_FROM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let spec = openmig_testkit::postgres_spec();
    let server_a = PostgresServer::start(&spec).await.unwrap();
    let server_b = PostgresServer::start(&spec).await.unwrap();
    let (url_a, url_b) = (url(&server_a, "dialo"), url(&server_b, "dialo"));
    recreate(&server_a, "dialo").await;
    recreate(&server_b, "dialo").await;
    let scope = Scope {
        manage_roles: true,
        ..Scope::default()
    };
    let settings = PgSettings::default();
    let mut failures: Vec<String> = Vec::new();
    let started = Instant::now();
    for (i, file) in files.iter().enumerate() {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        run_file(&url_b, file)
            .await
            .unwrap_or_else(|e| panic!("replaying {name}: {e}"));
        if i < from {
            run_file(&url_a, file)
                .await
                .unwrap_or_else(|e| panic!("replaying {name}: {e}"));
            continue;
        }
        let b = session(&url_b).await.inspect(&scope).await.unwrap();
        let a = session(&url_a).await.inspect(&scope).await.unwrap();
        let result = async {
            let planning = Instant::now();
            let plan = diff_and_plan(
                &a,
                &b,
                &scope,
                &settings,
                &RenameSet::default(),
                PlanMode::Generate,
                false,
            )
            .map_err(|e| format!("planning failed: {e}"))?;
            let planned_in = planning.elapsed();
            let db = crate::common::TestDb {
                name: "dialo".into(),
                url: url_a.clone(),
            };
            apply(&db, &plan)
                .await
                .map_err(|e| format!("applying failed: {e}"))?;
            let after = session(&url_a).await.inspect(&scope).await.unwrap();
            let again = diff_and_plan(
                &after,
                &b,
                &scope,
                &settings,
                &RenameSet::default(),
                PlanMode::Generate,
                false,
            )
            .map_err(|e| format!("planning again failed: {e}"))?;
            if !again.is_empty() {
                return Err(format!(
                    "does not converge.\nplan:\n{}\nremaining:\n{}",
                    plan.to_sql(),
                    again.to_sql()
                ));
            }
            if comparable(&after, &scope) != comparable(&b, &scope) {
                return Err("models differ after applying".into());
            }
            Ok((plan.len(), planned_in))
        }
        .await;
        match result {
            Ok((n, t)) => eprintln!("{i:3} {name}: ok ({n} statements, planned in {t:?})"),
            Err(e) => {
                eprintln!("{i:3} {name}: FAILED\n{e}");
                failures.push(format!("{name}: {}", e.lines().next().unwrap_or_default()));
                recreate(&server_a, "dialo").await;
                for f in &files[..=i] {
                    run_file(&url_a, f).await.unwrap();
                }
            }
        }
    }
    let checked = files.len() - from.min(files.len());
    eprintln!(
        "{} of {checked} migrations converged in {:?}",
        checked - failures.len(),
        started.elapsed()
    );

    // The final schema through HCL: encode, decode, build in a pristine database as `normalize`
    // does, and compare.
    let started = Instant::now();
    let b = session(&url_b).await.inspect(&scope).await.unwrap();
    let decoded = crate::plan::decoded_from(&b).model;
    recreate(&server_a, "dialo").await;
    let mut scratch = PgSession::connect(server_a.url(), &settings, SessionPurpose::Scratch)
        .await
        .unwrap();
    let pristine = scratch.inspect(&scope).await.unwrap();
    let plan = diff_and_plan(
        &pristine,
        &decoded,
        &scope,
        &settings,
        &RenameSet::default(),
        PlanMode::Scratch,
        false,
    )
    .unwrap();
    let retried = crate::plan::apply_with_retry(&mut scratch, &plan)
        .await
        .unwrap_or_else(|e| panic!("scratch build of the final schema failed: {e}"));
    let built = scratch.inspect(&scope).await.unwrap();
    eprintln!(
        "scratch build of the final schema: {} statements, {retried} retried, {:?}",
        plan.len(),
        started.elapsed()
    );
    if comparable(&built, &scope) != comparable(&b, &scope) {
        failures.push("the scratch build of the final schema differs".into());
        let again = diff_and_plan(
            &built,
            &b,
            &scope,
            &settings,
            &RenameSet::default(),
            PlanMode::Generate,
            false,
        );
        match again {
            Ok(p) => eprintln!(
                "scratch build differs; plan to the replayed schema:\n{}",
                p.to_sql()
            ),
            Err(e) => eprintln!("scratch build differs; planning failed: {e}"),
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
