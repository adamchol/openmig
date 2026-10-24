//! Smoke test: replay a real-world ClickHouse migration directory (Replicated databases,
//! `ON CLUSTER '{cluster}'`, TimeSeries, roles and grants) into a scratch server and inspect it.
//!
//! Ignored by default; it runs only when the directory exists:
//! `cargo test -p openmig-clickhouse --test dialo -- --ignored --nocapture`. The directory
//! defaults to the dialo checkout and can be set with `OPENMIG_SMOKE_CH_MIGRATIONS`.

mod common;

use common::connect;
use openmig_clickhouse::privileges::PrivilegeCatalog;
use openmig_clickhouse::{ChSession, ChSettings, Model};
use openmig_core::sql::split;
use openmig_core::{Dialect, Scope, Session};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_DIR: &str = "/home/coder/dialo/dialo-db/ch/migrations";

fn migration_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();
    files
}

/// Drop privileges this server does not know from `GRANT`/`REVOKE` statements (migrations
/// generated against another server version may name privileges that do not exist here).
fn adapt(sql: &str, catalog: &PrivilegeCatalog, dropped: &mut BTreeSet<String>) -> String {
    let Some(rest) = sql
        .strip_prefix("GRANT ")
        .or_else(|| sql.strip_prefix("REVOKE "))
    else {
        return sql.to_owned();
    };
    let verb = &sql[..sql.len() - rest.len()];
    let (cluster, rest) = match rest.strip_prefix("ON CLUSTER ") {
        Some(after) => match after.split_once(' ') {
            Some((name, rest)) => (format!("ON CLUSTER {name} "), rest),
            None => return sql.to_owned(),
        },
        None => (String::new(), rest),
    };
    let Some((privileges, target)) = rest.split_once(" ON ") else {
        return sql.to_owned();
    };
    let kept: Vec<&str> = privileges
        .split(", ")
        .filter(|p| {
            let known = catalog.canonical(p).is_some();
            if !known {
                dropped.insert((*p).to_owned());
            }
            known
        })
        .collect();
    format!("{verb}{cluster}{} ON {target}", kept.join(", "))
}

/// Apply every file statement by statement; returns the failures.
async fn replay(session: &mut ChSession, files: &[PathBuf]) -> Vec<String> {
    let catalog = session.privileges().await.unwrap().clone();
    let mut dropped = BTreeSet::new();
    let mut failures = Vec::new();
    for file in files {
        let sql = std::fs::read_to_string(file).unwrap();
        let statements = split(&sql, Dialect::ClickHouse).unwrap();
        let started = Instant::now();
        for statement in &statements {
            let sql = adapt(&statement.sql, &catalog, &mut dropped);
            if let Err(error) = session.execute(&sql).await {
                failures.push(format!("{}:{}: {error}", file.display(), statement.line));
            }
        }
        eprintln!(
            "  {}: {} statements in {:?}",
            file.file_name().unwrap().to_string_lossy(),
            statements.len(),
            started.elapsed()
        );
    }
    if !dropped.is_empty() {
        eprintln!(
            "  dropped privileges unknown to this server: {}",
            dropped.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    failures
}

fn summary(model: &Model) -> String {
    let mut out = String::new();
    for (name, db) in &model.databases {
        out.push_str(&format!(
            "  database {name} ({}): {} tables, {} views, {} materialized views\n",
            db.engine,
            db.tables.len(),
            db.views.len(),
            db.materialized_views.len()
        ));
    }
    out.push_str(&format!(
        "  {} roles, {} users, {} grants, {} role grants, {} dependencies\n",
        model.roles.len(),
        model.users.len(),
        model.grants.len(),
        model.role_grants.len(),
        model.dependencies.len()
    ));
    for grant in &model.grants {
        out.push_str(&format!(
            "  grant {} on {}{}: {}\n",
            grant.grantee,
            grant.target_sql(),
            if grant.grant_option {
                " with grant option"
            } else {
                ""
            },
            grant
                .privileges
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a real-world migration directory; run with --ignored"]
async fn replay_and_inspect() {
    let dir = std::env::var("OPENMIG_SMOKE_CH_MIGRATIONS").unwrap_or_else(|_| DEFAULT_DIR.into());
    let dir = Path::new(&dir);
    if !dir.is_dir() {
        eprintln!("skipping: {} does not exist", dir.display());
        return;
    }
    let files = migration_files(dir);
    let lease = openmig_testkit::clickhouse().await;
    let settings = ChSettings {
        cluster: Some("{cluster}".into()),
        ..ChSettings::default()
    };
    let mut session = connect(&lease, &settings).await;
    let scope = Scope {
        manage_roles: true,
        ..Scope::default()
    };

    let started = Instant::now();
    eprintln!("replaying {} files from {}", files.len(), dir.display());
    let failures = replay(&mut session, &files).await;
    eprintln!("replay: {:?}", started.elapsed());

    let started = Instant::now();
    let model = session.inspect(&scope).await.unwrap();
    eprintln!("inspect: {:?}\n{}", started.elapsed(), summary(&model));
    if std::env::var_os("OPENMIG_SMOKE_PRINT").is_some() {
        eprintln!("{}", common::compact(&model));
    }

    session.reset().await.unwrap();
    let second = replay(&mut session, &files).await;
    let again = session.inspect(&scope).await.unwrap();
    assert_eq!(again, model, "replaying twice must inspect to equal models");
    assert_eq!(second, failures);
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
