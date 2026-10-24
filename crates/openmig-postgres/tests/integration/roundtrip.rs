//! `encode` → load → `decode` of inspected models gives the inspected model back.

use crate::common::{ROLES, TestDb, admin, drop_role, drop_roles, run, with_prefix};
use openmig_core::{DataTable, EncodeOptions, EncodedFile, Engine, Scope, SplitBy};
use openmig_hcl::{LoadOptions, load_sources};
use openmig_postgres::model::Schema;
use openmig_postgres::{Database, PgSettings, PostgresEngine};
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::time::Instant;

const RICH_ROLES: &str = include_str!("sql/rich_roles.sql");
const RICH: &str = include_str!("sql/rich.sql");
const RICH_15: &str = include_str!("sql/rich_15.sql");
const RICH_17: &str = include_str!("sql/rich_17.sql");
const RICH_18: &str = include_str!("sql/rich_18.sql");

/// Encode, load and decode `model`.
fn round_trip(
    model: &Database,
    settings: &PgSettings,
    split: SplitBy,
) -> (Vec<EncodedFile>, Database) {
    let files = PostgresEngine::encode(model, settings, &EncodeOptions { split });
    let spec = PostgresEngine::schema_spec(settings);
    let sources = files
        .iter()
        .map(|f| (f.path.clone(), f.content.clone()))
        .collect();
    let opts = LoadOptions {
        spec: &spec,
        vars: Default::default(),
    };
    let doc = load_sources(sources, &opts).unwrap_or_else(|e| {
        panic!(
            "encoded HCL does not load:\n{}\n{}",
            e.render(false),
            dump(&files)
        )
    });
    let desired = PostgresEngine::decode(&doc, settings)
        .unwrap_or_else(|e| panic!("encoded HCL does not decode:\n{e}\n{}", dump(&files)));
    (files, desired.model)
}

fn dump(files: &[EncodedFile]) -> String {
    files
        .iter()
        .map(|f| format!("# {}\n{}", f.path.display(), f.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn view_query(q: &str) -> String {
    q.trim().trim_end_matches(';').trim_end().to_owned()
}

/// Drops what decoding does not reproduce: owners, dependencies, the server version, view column
/// types (views declare only commented columns), formatting of view queries and comments of
/// indexes that implement constraints.
fn comparable(mut db: Database) -> Database {
    db.dependencies.clear();
    db.server_major = 0;
    db.external_roles.clear();
    for e in db.event_triggers.values_mut() {
        e.owner.clear();
    }
    for s in db.schemas.values_mut() {
        comparable_schema(s);
    }
    db
}

fn comparable_schema(s: &mut Schema) {
    s.owner.clear();
    let view_columns = |columns: &mut Vec<openmig_postgres::model::ViewColumn>| {
        columns.retain(|c| c.comment.is_some() || !c.acl.is_empty());
        for c in columns.iter_mut() {
            c.data_type.clear();
        }
    };
    for t in s.tables.values_mut() {
        t.owner.clear();
        for i in t.indexes.values_mut() {
            if i.constraint {
                i.comment = None;
            }
        }
    }
    for v in s.views.values_mut() {
        v.owner.clear();
        v.query = view_query(&v.query);
        view_columns(&mut v.columns);
    }
    for m in s.materialized_views.values_mut() {
        m.owner.clear();
        m.query = view_query(&m.query);
        view_columns(&mut m.columns);
    }
    for f in s.functions.values_mut().chain(s.procedures.values_mut()) {
        f.owner.clear();
    }
    for q in s.sequences.values_mut() {
        q.owner.clear();
    }
    for e in s.enums.values_mut() {
        e.owner.clear();
    }
    for c in s.composites.values_mut() {
        c.owner.clear();
    }
    for d in s.domains.values_mut() {
        d.owner.clear();
    }
}

fn assert_round_trips(model: &Database, settings: &PgSettings, split: SplitBy) -> Vec<EncodedFile> {
    let (files, decoded) = round_trip(model, settings, split);
    assert_eq!(
        comparable(decoded),
        comparable(model.clone()),
        "round trip differs ({split:?}):\n{}",
        dump(&files)
    );
    files
}

fn data_table(schema: &str, table: &str) -> DataTable {
    DataTable {
        path: vec![schema.into(), table.into()],
        columns: Vec::new(),
        mode: None,
    }
}

#[tokio::test]
async fn rich_schema_round_trips() {
    let _roles = ROLES.read().await;
    let prefix = "omt_rt_";
    let Some(db) = TestDb::create("omt_rt").await else {
        return;
    };
    drop_roles(prefix).await;
    run(&mut admin().await, &with_prefix(RICH_ROLES, prefix)).await;
    let major = db.major().await;
    let mut sql = RICH.to_owned();
    for (min, part) in [(15, RICH_15), (17, RICH_17), (18, RICH_18)] {
        if major >= min {
            sql.push_str(part);
        }
    }
    db.run(&with_prefix(&sql, prefix)).await;
    let scope = Scope {
        manage_roles: true,
        data_tables: vec![data_table("app", "statuses")],
        data_max_rows: 100,
        ..Scope::default()
    };
    let mut model = db.inspect(&scope).await;
    model.roles.retain(|name, _| name.starts_with(prefix));
    let settings = PgSettings::default();
    let files = assert_round_trips(&model, &settings, SplitBy::None);
    assert_eq!(files.len(), 1);
    insta::with_settings!({ snapshot_suffix => format!("pg{major}"), prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!("rich_schema_hcl", files[0].content);
    });
    let by_schema = assert_round_trips(&model, &settings, SplitBy::Schema);
    let names: Vec<String> = by_schema
        .iter()
        .map(|f| f.path.display().to_string())
        .collect();
    let mut expected = vec!["app.hcl", "audit.hcl", "cluster.hcl"];
    if major < 15 {
        // `public` grants CREATE to PUBLIC before PostgreSQL 15, which is not a default.
        expected.push("public.hcl");
    }
    assert_eq!(names, expected);
    assert_round_trips(&model, &settings, SplitBy::Kind);
    db.drop().await;
    drop_roles(prefix).await;
}

fn created_roles(sql: &str) -> Vec<String> {
    sql.match_indices("CREATE ROLE \"")
        .filter_map(|(i, m)| {
            let rest = &sql[i + m.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

/// Smoke test on a real migration directory:
/// `OPENMIG_DIALO_MIGRATIONS=<dir> cargo test -p openmig-postgres -- --ignored --nocapture
/// dialo_round_trip`. Replays the migrations with `psql`, inspects with roles managed, encodes
/// one file per schema, loads and decodes, and compares. Nothing of the inspected database is
/// written to disk unless `OPENMIG_DIALO_HCL=<dir>` is set.
#[tokio::test]
#[ignore = "needs OPENMIG_DIALO_MIGRATIONS and psql"]
async fn dialo_round_trip() {
    let Some(dir) = std::env::var_os("OPENMIG_DIALO_MIGRATIONS").map(PathBuf::from) else {
        eprintln!("skipping: set OPENMIG_DIALO_MIGRATIONS to a migration directory");
        return;
    };
    let _roles = ROLES.read().await;
    let Some(db) = TestDb::create("omt_dialo_rt").await else {
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
    let scope = Scope {
        manage_roles: true,
        ..Scope::default()
    };
    let started = Instant::now();
    let mut model = db.inspect(&scope).await;
    let inspected = started.elapsed();
    model.roles.retain(|name, _| roles.contains(name));
    let settings = PgSettings::default();
    let started = Instant::now();
    let encoded = PostgresEngine::encode(
        &model,
        &settings,
        &EncodeOptions {
            split: SplitBy::Schema,
        },
    );
    let encode_time = started.elapsed();
    let started = Instant::now();
    let (_, decoded) = round_trip(&model, &settings, SplitBy::Schema);
    let load_decode_time = started.elapsed().saturating_sub(encode_time);
    if let Some(out) = std::env::var_os("OPENMIG_DIALO_HCL").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        for f in &encoded {
            std::fs::write(out.join(&f.path), &f.content).unwrap();
        }
    }
    let lines: usize = encoded.iter().map(|f| f.content.lines().count()).sum();
    eprintln!(
        "inspect {inspected:?}, encode {encode_time:?}, load + decode {load_decode_time:?}; {} \
         files, {lines} lines",
        encoded.len()
    );
    let (left, right) = (comparable(decoded), comparable(model));
    db.drop().await;
    for role in &roles {
        drop_role(role).await;
    }
    assert_eq!(left, right, "dialo round trip differs");
}
