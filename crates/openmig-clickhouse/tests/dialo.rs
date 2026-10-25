//! Acceptance tests on a real-world ClickHouse setup (Replicated databases,
//! `ON CLUSTER '{cluster}'`, TimeSeries, roles and grants):
//!
//! * `replay_and_inspect`: replay the migration directory into a scratch server and inspect it.
//! * `inspect_output_round_trips`: replay, inspect, encode as HCL, load and decode it, normalize
//!   it on a fresh server; the plan between the two must be empty. With a translation of the
//!   project's own schema files (`OPENMIG_SMOKE_CH_SCHEMA`, a directory of `.hcl` files that is
//!   not part of this repository), that schema is normalized too and every remaining difference
//!   to the replayed migrations is printed.
//!
//! Ignored by default; they run only when the directory exists:
//! `cargo test -p openmig-clickhouse --test dialo -- --ignored --nocapture`. The directory
//! defaults to the dialo checkout and can be set with `OPENMIG_SMOKE_CH_MIGRATIONS`.

mod common;

use common::connect;
use openmig_clickhouse::privileges::PrivilegeCatalog;
use openmig_clickhouse::{ChSession, ChSettings, ClickHouseEngine, Model};
use openmig_core::sql::split;
use openmig_core::{
    ChangeInfo, Dialect, DiffContext, EncodeOptions, Engine, ExecErrorKind, PlanMode, RenameSet,
    Scope, Session,
};
use openmig_hcl::{LoadOptions, load_sources};
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

fn scope() -> Scope {
    Scope {
        manage_roles: true,
        ..Scope::default()
    }
}

fn context<'a>(
    settings: &'a ChSettings,
    scope: &'a Scope,
    renames: &'a RenameSet,
    mode: PlanMode,
) -> DiffContext<'a, ClickHouseEngine> {
    DiffContext {
        settings,
        scope,
        renames,
        mode,
        allow_recreate: false,
    }
}

/// Load and decode HCL sources, apply them to the reset server (retrying statements that
/// reference objects created later) and inspect the result.
async fn normalize(
    session: &mut ChSession,
    settings: &ChSettings,
    sources: Vec<(PathBuf, String)>,
) -> Model {
    let spec = ClickHouseEngine::schema_spec(settings);
    let doc = load_sources(
        sources,
        &LoadOptions {
            spec: &spec,
            vars: Default::default(),
        },
    )
    .unwrap_or_else(|e| panic!("{}", e.render(false)));
    let desired = ClickHouseEngine::decode(&doc, settings).unwrap_or_else(|e| panic!("{e}"));
    session.reset().await.unwrap();
    let scope = scope();
    let renames = RenameSet::default();
    let ctx = context(settings, &scope, &renames, PlanMode::Scratch);
    let pristine = session.inspect(&scope).await.unwrap();
    let changes = ClickHouseEngine::diff(&pristine, &desired.model, &ctx).unwrap();
    let plan = ClickHouseEngine::plan(changes, &pristine, &desired.model, &ctx).unwrap();
    let mut queue: Vec<String> = plan.statements.into_iter().map(|s| s.sql).collect();
    while !queue.is_empty() {
        let before = queue.len();
        let mut deferred = Vec::new();
        for sql in queue {
            match session.execute(&sql).await {
                Ok(()) => {}
                Err(e) if e.kind == ExecErrorKind::UndefinedObject => deferred.push(sql),
                Err(e) => panic!("{sql}\n{e}"),
            }
        }
        assert!(
            deferred.len() < before,
            "cannot apply:\n{}",
            deferred.join("\n")
        );
        queue = deferred;
    }
    session.inspect(&scope).await.unwrap()
}

/// Summaries of the changes from `current` to `desired`.
fn differences(settings: &ChSettings, current: &Model, desired: &Model) -> Vec<String> {
    let scope = scope();
    let renames = RenameSet::default();
    let ctx = context(settings, &scope, &renames, PlanMode::Generate);
    match ClickHouseEngine::diff(current, desired, &ctx) {
        Ok(changes) => changes.iter().map(ChangeInfo::summary).collect(),
        Err(e) => vec![format!("error: {e}")],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a real-world migration directory; run with --ignored"]
async fn inspect_output_round_trips() {
    let dir = std::env::var("OPENMIG_SMOKE_CH_MIGRATIONS").unwrap_or_else(|_| DEFAULT_DIR.into());
    let dir = Path::new(&dir);
    if !dir.is_dir() {
        eprintln!("skipping: {} does not exist", dir.display());
        return;
    }
    let lease = openmig_testkit::clickhouse().await;
    let settings = ChSettings {
        cluster: Some("{cluster}".into()),
        ..ChSettings::default()
    };
    let mut session = connect(&lease, &settings).await;
    let failures = replay(&mut session, &migration_files(dir)).await;
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
    let replayed = session.inspect(&scope()).await.unwrap();

    let started = Instant::now();
    let files = ClickHouseEngine::encode(&replayed, &settings, &EncodeOptions::default());
    let hcl: String = files.iter().map(|f| f.content.clone()).collect();
    if std::env::var_os("OPENMIG_SMOKE_PRINT").is_some() {
        eprintln!("{hcl}");
    }
    let sources = files.into_iter().map(|f| (f.path, f.content)).collect();
    let normalized = normalize(&mut session, &settings, sources).await;
    let remaining = differences(&settings, &replayed, &normalized);
    eprintln!(
        "encode + decode + normalize: {:?}, {} lines of HCL",
        started.elapsed(),
        hcl.lines().count()
    );
    assert!(
        remaining.is_empty(),
        "inspect output does not round-trip:\n{}\n{hcl}",
        remaining.join("\n")
    );

    let schema = std::env::var("OPENMIG_SMOKE_CH_SCHEMA")
        .unwrap_or_else(|_| "/tmp/openmig-dialo-ch-hcl".into());
    let schema = Path::new(&schema);
    if !schema.is_dir() {
        eprintln!(
            "no translated schema at {}; skipping the comparison",
            schema.display()
        );
        return;
    }
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(schema).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "hcl") {
            let text = std::fs::read_to_string(&path).unwrap();
            sources.push((path, text));
        }
    }
    sources.sort();
    let declared = normalize(&mut session, &settings, sources).await;
    let differences = differences(&settings, &replayed, &declared);
    eprintln!(
        "translated schema vs replayed migrations: {} differences",
        differences.len()
    );
    for d in &differences {
        eprintln!("  {d}");
    }
}
