use crate::ClickHouseEngine;
use crate::model::*;
use crate::settings::ChSettings;
use openmig_core::{DiffContext, Engine, Error, ObjectKind, ObjectRef, PlanMode, RenameSet, Scope};
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;

struct Options {
    settings: ChSettings,
    mode: PlanMode,
    allow_recreate: bool,
    renames: RenameSet,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            settings: ChSettings::default(),
            mode: PlanMode::Generate,
            allow_recreate: false,
            renames: RenameSet::default(),
        }
    }
}

fn plan_with(current: &Model, desired: &Model, options: &Options) -> Result<Vec<String>, Error> {
    let scope = Scope::default();
    let ctx = DiffContext::<ClickHouseEngine> {
        settings: &options.settings,
        scope: &scope,
        renames: &options.renames,
        mode: options.mode,
        allow_recreate: options.allow_recreate,
    };
    let changes = ClickHouseEngine::diff(current, desired, &ctx)?;
    let plan = ClickHouseEngine::plan(changes, current, desired, &ctx)?;
    Ok(plan.statements.into_iter().map(|s| s.sql).collect())
}

fn plan(current: &Model, desired: &Model) -> Vec<String> {
    plan_with(current, desired, &Options::default()).unwrap()
}

fn base() -> Model {
    let mut m = Model::default();
    m.databases
        .insert("default".into(), Database::new("default", "Atomic"));
    m
}

fn mergetree(name: &str, columns: &[(&str, &str)]) -> Table {
    let mut t = Table::new(name, "MergeTree");
    t.columns = columns.iter().map(|(n, ty)| Column::new(*n, *ty)).collect();
    t.order_by = vec![columns[0].0.to_owned()];
    t.primary_key = t.order_by.clone();
    t.settings.insert("index_granularity".into(), "8192".into());
    t
}

fn with_db(mut m: Model, db: Database) -> Model {
    m.databases.insert(db.name.clone(), db);
    m
}

/// `app.src` → materialized view `app.copy` → `app.dst`.
fn pipeline(src_engine: &str) -> Model {
    let mut db = Database::new("app", "Atomic");
    let mut src = mergetree("src", &[("x", "UInt32")]);
    src.engine = src_engine.to_owned();
    db.tables.insert("src".into(), src);
    db.tables
        .insert("dst".into(), mergetree("dst", &[("x", "UInt32")]));
    db.materialized_views.insert(
        "copy".into(),
        MaterializedView {
            name: "copy".into(),
            to: Some(QualifiedName::new("app", "dst")),
            query: "SELECT x FROM app.src".into(),
            columns: vec![Column::new("x", "UInt32")],
            ..MaterializedView::default()
        },
    );
    let mut m = with_db(base(), db);
    let mv = ObjectRef::new(ObjectKind::MaterializedView, ["app", "copy"]);
    m.dependencies = vec![
        Dependency {
            from: mv.clone(),
            to: ObjectRef::new(ObjectKind::Table, ["app", "dst"]),
            soft: false,
        },
        Dependency {
            from: mv,
            to: ObjectRef::new(ObjectKind::Table, ["app", "src"]),
            soft: false,
        },
    ];
    m
}

fn first_words(statements: &[String]) -> Vec<String> {
    statements
        .iter()
        .map(|s| s.split_whitespace().take(4).collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn creates_in_dependency_order_and_drops_in_reverse() {
    let desired = pipeline("MergeTree");
    let statements = plan(&base(), &desired);
    assert_eq!(
        first_words(&statements),
        [
            "CREATE DATABASE IF NOT",
            "CREATE TABLE IF NOT",
            "CREATE TABLE IF NOT",
            "CREATE MATERIALIZED VIEW IF",
        ]
    );
    assert!(statements[1].contains("`app`.`dst`"));
    let mut without_db = desired.clone();
    without_db.databases.remove("app");
    let mut gone = pipeline("MergeTree");
    gone.databases.get_mut("app").unwrap().tables.remove("src");
    gone.databases
        .get_mut("app")
        .unwrap()
        .materialized_views
        .remove("copy");
    let drops = plan(&desired, &gone);
    assert_eq!(
        drops,
        [
            "DROP VIEW IF EXISTS `app`.`copy` SYNC",
            "DROP TABLE IF EXISTS `app`.`src` SYNC"
        ]
    );
    assert_eq!(
        plan(&desired, &without_db),
        ["DROP DATABASE IF EXISTS `app` SYNC"]
    );
}

#[test]
fn unsupported_changes_and_recreate_with_dependants() {
    let current = pipeline("MergeTree");
    let desired = pipeline("ReplacingMergeTree");
    let err = plan_with(&current, &desired, &Options::default()).unwrap_err();
    let Error::Unsupported { object, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(object.dotted(), "app.src");
    assert!(reason.contains("engine"), "{reason}");

    let statements = plan_with(
        &current,
        &desired,
        &Options {
            allow_recreate: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(
        first_words(&statements),
        [
            "DROP VIEW IF EXISTS",
            "DROP TABLE IF EXISTS",
            "CREATE TABLE IF NOT",
            "CREATE MATERIALIZED VIEW IF",
        ]
    );
    assert!(statements[2].contains("ENGINE = ReplacingMergeTree"));
}

#[test]
fn renaming_a_source_recreates_its_materialized_views() {
    let current = pipeline("MergeTree");
    let mut desired = pipeline("MergeTree");
    let app = desired.databases.get_mut("app").unwrap();
    let mut src = app.tables.remove("src").unwrap();
    src.name = "source".into();
    app.tables.insert("source".into(), src);
    app.materialized_views.get_mut("copy").unwrap().query = "SELECT x FROM app.source".into();
    let mut renames = RenameSet::default();
    renames.push(
        ObjectRef::new(ObjectKind::Table, ["app", "src"]),
        ObjectRef::new(ObjectKind::Table, ["app", "source"]),
    );
    let statements = plan_with(
        &current,
        &desired,
        &Options {
            renames,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(
        statements,
        [
            "DROP VIEW IF EXISTS `app`.`copy` SYNC",
            "RENAME TABLE `app`.`src` TO `app`.`source`",
            "CREATE MATERIALIZED VIEW IF NOT EXISTS `app`.`copy` TO `app`.`dst` AS\nSELECT x FROM app.source",
        ]
    );
}

#[test]
fn materialized_view_query_changes() {
    let current = pipeline("MergeTree");
    let mut desired = pipeline("MergeTree");
    let mv = desired
        .databases
        .get_mut("app")
        .unwrap()
        .materialized_views
        .get_mut("copy")
        .unwrap();
    mv.query = "SELECT x * 2 AS x FROM app.src".into();
    mv.columns = vec![Column::new("x", "UInt64")];
    assert_eq!(
        plan(&current, &desired),
        ["ALTER TABLE `app`.`copy` MODIFY QUERY\nSELECT x * 2 AS x FROM app.src"]
    );
    let mut stale = pipeline("MergeTree");
    stale
        .databases
        .get_mut("app")
        .unwrap()
        .materialized_views
        .get_mut("copy")
        .unwrap()
        .columns = vec![Column::new("x", "UInt64")];
    assert_eq!(
        first_words(&plan(&current, &stale)),
        ["DROP VIEW IF EXISTS", "CREATE MATERIALIZED VIEW IF"]
    );
}

#[test]
fn on_cluster_rules() {
    let mut repl = Database::new(
        "repl",
        "Replicated('/clickhouse/databases/repl', '{shard}', '{replica}')",
    );
    repl.tables
        .insert("t".into(), mergetree("t", &[("id", "UInt64")]));
    let mut plain = Database::new("plain", "Atomic");
    plain
        .tables
        .insert("t".into(), mergetree("t", &[("id", "UInt64")]));
    let mut desired = with_db(with_db(base(), repl), plain);
    for (name, storage) in [("local", "local_directory"), ("shared", "replicated")] {
        desired.roles.insert(
            name.into(),
            Role {
                name: name.into(),
                storage: storage.into(),
            },
        );
        desired.grants.push(Grant {
            grantee: Grantee::Role(name.into()),
            database: Some("plain".into()),
            privileges: BTreeSet::from(["SELECT".to_owned()]),
            ..Grant::default()
        });
    }
    desired.functions.insert(
        "f".into(),
        Function {
            name: "f".into(),
            parameters: vec!["x".into()],
            body: "x".into(),
        },
    );
    let options = Options {
        settings: ChSettings {
            cluster: Some("{cluster}".into()),
            ..ChSettings::default()
        },
        ..Options::default()
    };
    let statements = plan_with(&base(), &desired, &options).unwrap();
    let on_cluster: Vec<(String, bool)> = statements
        .iter()
        .map(|s| {
            (
                s.lines().next().unwrap().to_owned(),
                s.contains("ON CLUSTER `{cluster}`"),
            )
        })
        .collect();
    assert_eq!(
        on_cluster,
        [
            (
                "CREATE ROLE IF NOT EXISTS `local` ON CLUSTER `{cluster}` IN `local_directory`"
                    .to_owned(),
                true
            ),
            (
                "CREATE ROLE IF NOT EXISTS `shared` IN `replicated`".to_owned(),
                false
            ),
            (
                "CREATE DATABASE IF NOT EXISTS `plain` ON CLUSTER `{cluster}` ENGINE = Atomic"
                    .to_owned(),
                true
            ),
            (
                "CREATE DATABASE IF NOT EXISTS `repl` ON CLUSTER `{cluster}` ENGINE = Replicated('/clickhouse/databases/repl', '{shard}', '{replica}')".to_owned(),
                true
            ),
            (
                "CREATE OR REPLACE FUNCTION `f` ON CLUSTER `{cluster}` AS (`x`) -> x".to_owned(),
                true
            ),
            (
                "CREATE TABLE IF NOT EXISTS `plain`.`t` ON CLUSTER `{cluster}`".to_owned(),
                true
            ),
            ("CREATE TABLE IF NOT EXISTS `repl`.`t`".to_owned(), false),
            (
                "GRANT ON CLUSTER `{cluster}` SELECT ON `plain`.* TO `local`".to_owned(),
                true
            ),
            ("GRANT SELECT ON `plain`.* TO `shared`".to_owned(), false),
        ]
    );
}

#[test]
fn grants_revoke_before_grant() {
    let mut current = base();
    current.roles.insert(
        "web".into(),
        Role {
            name: "web".into(),
            storage: "local_directory".into(),
        },
    );
    let mut desired = current.clone();
    let grant = |privileges: &[&str], grant_option: bool| Grant {
        grantee: Grantee::Role("web".into()),
        database: Some("app".into()),
        privileges: privileges.iter().map(|p| (*p).to_owned()).collect(),
        grant_option,
        ..Grant::default()
    };
    current.grants = vec![grant(&["ALL"], false), grant(&["dictGet"], true)];
    desired.grants = vec![grant(&["SELECT", "dictGet"], false)];
    assert_eq!(
        plan(&current, &desired),
        [
            "REVOKE ALL ON `app`.* FROM `web`",
            "REVOKE GRANT OPTION FOR dictGet ON `app`.* FROM `web`",
            "GRANT SELECT ON `app`.* TO `web`",
        ]
    );
    assert_eq!(
        plan(&desired, &current),
        [
            "GRANT dictGet ON `app`.* TO `web` WITH GRANT OPTION",
            "GRANT ALL ON `app`.* TO `web`",
        ]
    );
}

#[test]
fn scratch_builds_create_external_roles() {
    let mut desired = base();
    desired.external_roles = BTreeSet::from(["outside".to_owned()]);
    desired.grants.push(Grant {
        grantee: Grantee::Role("outside".into()),
        privileges: BTreeSet::from(["SELECT".to_owned()]),
        ..Grant::default()
    });
    let scratch = plan_with(
        &base(),
        &desired,
        &Options {
            mode: PlanMode::Scratch,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(
        scratch,
        [
            "CREATE ROLE IF NOT EXISTS `outside`",
            "GRANT SELECT ON *.* TO `outside`"
        ]
    );
    assert_eq!(
        plan(&base(), &desired),
        ["GRANT SELECT ON *.* TO `outside`"]
    );
}

#[test]
fn destructive_statements_are_marked() {
    let mut db = Database::new("app", "Atomic");
    db.tables.insert(
        "t".into(),
        mergetree("t", &[("id", "UInt64"), ("legacy", "String")]),
    );
    let current = with_db(base(), db);
    let mut desired = current.clone();
    let t = desired
        .databases
        .get_mut("app")
        .unwrap()
        .tables
        .get_mut("t")
        .unwrap();
    t.columns.pop();
    t.comment = Some("c".into());
    let scope = Scope::default();
    let settings = ChSettings::default();
    let renames = RenameSet::default();
    let ctx = DiffContext::<ClickHouseEngine> {
        settings: &settings,
        scope: &scope,
        renames: &renames,
        mode: PlanMode::Sync,
        allow_recreate: false,
    };
    let changes = ClickHouseEngine::diff(&current, &desired, &ctx).unwrap();
    let plan = ClickHouseEngine::plan(changes, &current, &desired, &ctx).unwrap();
    let marked: Vec<bool> = plan
        .statements
        .iter()
        .map(|s| s.destructive.is_some())
        .collect();
    assert_eq!(marked, [false, true]);
    assert_eq!(
        plan.statements[1].origin,
        Some(ObjectRef::new(ObjectKind::Table, ["app", "t"]))
    );
    assert!(!plan.requires_no_transaction());
}
