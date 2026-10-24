//! Smoke test: inspect a database built by replaying a real migration directory.
//!
//! `OPENMIG_DIALO_MIGRATIONS=<dir> cargo test -p openmig-postgres -- --ignored --nocapture dialo`
//! replays `<dir>/*.sql` with `psql` into a fresh database, then inspects it with roles managed
//! and prints timings. Roles the migrations create are dropped before and after.

use crate::common::{ROLES, TestDb, drop_role};
use openmig_core::{Scope, Session};
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::time::Instant;

fn created_roles(sql: &str) -> Vec<String> {
    sql.match_indices("CREATE ROLE \"")
        .filter_map(|(i, m)| {
            let rest = &sql[i + m.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

#[tokio::test]
#[ignore = "needs OPENMIG_DIALO_MIGRATIONS and psql"]
async fn dialo_smoke() {
    let Some(dir) = std::env::var_os("OPENMIG_DIALO_MIGRATIONS").map(PathBuf::from) else {
        eprintln!("skipping: set OPENMIG_DIALO_MIGRATIONS to a migration directory");
        return;
    };
    if !dir.is_dir() {
        eprintln!("skipping: {} is not a directory", dir.display());
        return;
    }
    let _roles = ROLES.read().await;
    let Some(db) = TestDb::create("omt_dialo").await else {
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    let mut roles = Vec::new();
    for f in &files {
        roles.extend(created_roles(&std::fs::read_to_string(f).unwrap()));
    }
    for role in &roles {
        drop_role(role).await;
    }

    let started = Instant::now();
    for f in &files {
        let out = tokio::process::Command::new("psql")
            .args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", &db.url, "-f"])
            .arg(f)
            .output()
            .await
            .expect("run psql");
        assert!(
            out.status.success(),
            "{} failed:\n{}",
            f.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    eprintln!("replayed {} files in {:?}", files.len(), started.elapsed());

    let scope = Scope {
        manage_roles: true,
        ..Scope::default()
    };
    let mut session = db.session().await;
    let mut timings = Vec::new();
    let mut models = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        models.push(session.inspect(&scope).await.unwrap());
        timings.push(started.elapsed());
    }
    eprintln!("inspection times: {timings:?}");
    let model = &models[0];
    assert_eq!(model, &models[1]);
    let count = |f: &dyn Fn(&openmig_postgres::model::Schema) -> usize| -> usize {
        model.schemas.values().map(f).sum()
    };
    eprintln!(
        "schemas {}, tables {}, columns {}, indexes {}, constraints {}, policies {}, triggers {}, \
         views {}, functions {}, procedures {}, sequences {}, types {}, roles {}, extensions {}, \
         event triggers {}, dependencies {}",
        model.schemas.len(),
        count(&|s| s.tables.len()),
        count(&|s| s.tables.values().map(|t| t.columns.len()).sum()),
        count(&|s| s.tables.values().map(|t| t.indexes.len()).sum()),
        count(&|s| {
            s.tables
                .values()
                .map(|t| {
                    t.foreign_keys.len()
                        + t.uniques.len()
                        + t.checks.len()
                        + t.exclusions.len()
                        + usize::from(t.primary_key.is_some())
                })
                .sum()
        }),
        count(&|s| s.tables.values().map(|t| t.policies.len()).sum()),
        count(&|s| s.tables.values().map(|t| t.triggers.len()).sum()),
        count(&|s| s.views.len() + s.materialized_views.len()),
        count(&|s| s.functions.len()),
        count(&|s| s.procedures.len()),
        count(&|s| s.sequences.len()),
        count(&|s| s.enums.len() + s.composites.len() + s.domains.len()),
        model.roles.len(),
        model.extensions.len(),
        model.event_triggers.len(),
        model.dependencies.len(),
    );
    drop(session);
    db.drop().await;
    for role in &roles {
        drop_role(role).await;
    }
}
