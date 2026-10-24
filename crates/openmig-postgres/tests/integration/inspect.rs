use crate::common::{ROLES, TestDb, admin, drop_roles, run, with_prefix};
use openmig_core::{ExcludeSet, ObjectKind, ObjectRef, SchemaFilter, Scope, Session};
use openmig_postgres::Database;
use pretty_assertions::assert_eq;

const RICH_ROLES: &str = include_str!("sql/rich_roles.sql");
const RICH: &str = include_str!("sql/rich.sql");
const RICH_15: &str = include_str!("sql/rich_15.sql");
const RICH_17: &str = include_str!("sql/rich_17.sql");
const RICH_18: &str = include_str!("sql/rich_18.sql");
const ORDER_ROLES: &str = include_str!("sql/order_roles.sql");
const ORDER_A: &str = include_str!("sql/order_a.sql");
const ORDER_B: &str = include_str!("sql/order_b.sql");

async fn create_roles(sql: &str, prefix: &str) {
    drop_roles(prefix).await;
    run(&mut admin().await, &with_prefix(sql, prefix)).await;
}

/// Load the rich schema (and the parts that need a newer server).
async fn build_rich(db: &TestDb, prefix: &str) -> u32 {
    let major = db.major().await;
    let mut sql = RICH.to_owned();
    for (min, part) in [(15, RICH_15), (17, RICH_17), (18, RICH_18)] {
        if major >= min {
            sql.push_str(part);
        }
    }
    db.run(&with_prefix(&sql, prefix)).await;
    major
}

fn rich_scope() -> Scope {
    Scope {
        manage_roles: true,
        data_tables: vec![data_table("app", "statuses")],
        data_max_rows: 100,
        ..Scope::default()
    }
}

/// Roles are cluster-wide: keep only the ones a test created.
fn own_roles(mut model: Database, prefix: &str) -> Database {
    model.roles.retain(|name, _| name.starts_with(prefix));
    model
}

#[tokio::test]
async fn rich_schema_snapshot() {
    let _roles = ROLES.read().await;
    let prefix = "omt_rich_";
    let Some(db) = TestDb::create("omt_rich").await else {
        return;
    };
    create_roles(RICH_ROLES, prefix).await;
    let major = build_rich(&db, prefix).await;
    let model = own_roles(db.inspect(&rich_scope()).await, prefix);
    insta::with_settings!({ snapshot_suffix => format!("pg{major}"), prepend_module_to_snapshot => false }, {
        insta::assert_debug_snapshot!("rich_schema", model);
    });
    let mut again = db.session().await;
    let second = own_roles(again.inspect(&rich_scope()).await.unwrap(), prefix);
    assert_eq!(model, second, "a second inspection differs");
    for (object, _) in model.objects() {
        assert!(model.get(&object).is_some(), "{object} not found by get");
    }
    for d in &model.dependencies {
        assert!(model.contains(&d.from), "dangling dependency {d:?}");
        assert!(model.contains(&d.to), "dangling dependency {d:?}");
    }
    db.drop().await;
    drop_roles(prefix).await;
}

#[tokio::test]
async fn same_schema_in_two_databases_inspects_equal() {
    let _roles = ROLES.read().await;
    let prefix = "omt_same_";
    let Some(a) = TestDb::create("omt_same_a").await else {
        return;
    };
    let b = TestDb::create("omt_same_b").await.unwrap();
    create_roles(RICH_ROLES, prefix).await;
    // Objects created in between shift the OIDs of the second database.
    b.run("CREATE TABLE public.filler (id int); DROP TABLE public.filler;")
        .await;
    build_rich(&a, prefix).await;
    build_rich(&b, prefix).await;
    let ma = own_roles(a.inspect(&rich_scope()).await, prefix);
    let mb = own_roles(b.inspect(&rich_scope()).await, prefix);
    assert_eq!(ma, mb);
    a.drop().await;
    b.drop().await;
    drop_roles(prefix).await;
}

#[tokio::test]
async fn creation_order_does_not_matter() {
    let _roles = ROLES.read().await;
    let prefix = "omt_ord_";
    let Some(a) = TestDb::create("omt_ord_a").await else {
        return;
    };
    let b = TestDb::create("omt_ord_b").await.unwrap();
    create_roles(ORDER_ROLES, prefix).await;
    a.run(&with_prefix(ORDER_A, prefix)).await;
    b.run(&with_prefix(ORDER_B, prefix)).await;
    let scope = Scope {
        schemas: SchemaFilter::Only(vec!["ord".into()]),
        manage_roles: true,
        ..Scope::default()
    };
    let ma = own_roles(a.inspect(&scope).await, prefix);
    let mb = own_roles(b.inspect(&scope).await, prefix);
    assert_eq!(ma, mb);
    assert_eq!(ma.schemas.keys().collect::<Vec<_>>(), ["ord"]);
    assert_eq!(
        ma.schemas["ord"].enums["color"].values,
        ["red", "green", "blue"]
    );
    a.drop().await;
    b.drop().await;
    drop_roles(prefix).await;
}

fn mentions(model: &Database, needle: &str) -> bool {
    format!("{model:?}").contains(needle)
}

#[tokio::test]
async fn excludes_and_schema_filter() {
    let _roles = ROLES.read().await;
    let prefix = "omt_excl_";
    let Some(db) = TestDb::create("omt_excl").await else {
        return;
    };
    create_roles(RICH_ROLES, prefix).await;
    build_rich(&db, prefix).await;
    let scope = Scope {
        schemas: SchemaFilter::Only(vec!["app".into()]),
        exclude: ExcludeSet::new([
            "table:app.cache",
            "function:app.debug_*",
            "role:omt_excl_admin",
            "column:app.users.secret",
            "policy:app.users.users_admin",
            "index:app.users.users_tags_idx",
            "app.org_*",
            "event_trigger:*",
        ])
        .unwrap(),
        manage_roles: true,
        ..Scope::default()
    };
    let model = own_roles(db.inspect(&scope).await, prefix);
    assert_eq!(model.schemas.keys().collect::<Vec<_>>(), ["app"]);
    assert!(model.extensions.is_empty(), "extensions live in public");
    assert!(model.event_triggers.is_empty());
    let app = &model.schemas["app"];
    assert!(!app.tables.contains_key("cache"));
    assert!(app.tables.contains_key("orgs"));
    assert!(!app.views.contains_key("org_names"));
    assert!(!app.materialized_views.contains_key("org_stats"));
    assert!(
        !app.functions
            .keys()
            .any(|k| k.starts_with("debug_") || k.starts_with("org_"))
    );
    let users = &app.tables["users"];
    assert!(users.column("secret").is_none());
    assert!(!users.policies.contains_key("users_admin"));
    assert!(users.policies.contains_key("users_select"));
    assert!(!users.indexes.contains_key("users_tags_idx"));
    assert!(model.roles.contains_key("omt_excl_web"));
    assert!(!model.roles.contains_key("omt_excl_admin"));
    assert!(!mentions(&model, "grantee: \"omt_excl_admin\""));
    assert!(
        !model.roles["omt_excl_worker"]
            .member_of
            .contains_key("omt_excl_admin")
    );
    for d in &model.dependencies {
        assert!(model.contains(&d.from) && model.contains(&d.to), "{d:?}");
    }
    let without_roles = db
        .inspect(&Scope {
            manage_roles: false,
            ..scope.clone()
        })
        .await;
    assert!(without_roles.roles.is_empty());
    db.drop().await;
    drop_roles(prefix).await;
}

#[tokio::test]
async fn data_rows_and_limit() {
    let Some(db) = TestDb::create("omt_data").await else {
        return;
    };
    db.run(
        "CREATE TABLE public.kv (k text PRIMARY KEY, v numeric, at timestamptz, g int GENERATED ALWAYS AS (1) STORED);
         INSERT INTO public.kv VALUES ('b', 1.50, '2026-01-01 12:00+02'), ('a', NULL, NULL);
         CREATE TABLE public.nokey (a int, b text);
         INSERT INTO public.nokey VALUES (2, 'x'), (1, NULL);",
    )
    .await;
    let scope = Scope {
        data_tables: vec![
            data_table("public", "kv"),
            data_table("public", "nokey"),
            data_table("public", "missing"),
        ],
        data_max_rows: 2,
        ..Scope::default()
    };
    let model = db.inspect(&scope).await;
    insta::assert_debug_snapshot!("data_rows", model.data);
    let refused = db
        .session()
        .await
        .inspect(&Scope {
            data_max_rows: 1,
            ..scope
        })
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("data_max_rows"), "{refused}");
    db.drop().await;
}

#[tokio::test]
async fn inspection_restores_session_settings() {
    let Some(db) = TestDb::create("omt_settings").await else {
        return;
    };
    db.run("CREATE TABLE public.t (id int)").await;
    let mut session = db.session().await;
    session
        .execute("SET search_path = public; SET TimeZone = 'Europe/Warsaw'")
        .await
        .unwrap();
    let model = session.inspect(&Scope::default()).await.unwrap();
    assert!(model.contains(&ObjectRef::new(ObjectKind::Table, ["public", "t"])));
    assert!(!model.is_empty());
    let row = session
        .client()
        .query_one(
            "SELECT current_setting('search_path'), current_setting('TimeZone')",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "public");
    assert_eq!(row.get::<_, String>(1), "Europe/Warsaw");
    session.execute("DROP TABLE public.t").await.unwrap();
    assert!(session.inspect(&Scope::default()).await.unwrap().is_empty());
    db.drop().await;
}

/// No transaction block is open (`now()` is the start of the current statement).
async fn autocommit(s: &openmig_postgres::PgSession) -> bool {
    let rows = s
        .client()
        .simple_query("SELECT now() = statement_timestamp()")
        .await
        .unwrap();
    rows.iter()
        .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(r) if r.get(0) == Some("t")))
}

#[tokio::test]
async fn inspection_respects_open_transactions() {
    let Some(db) = TestDb::create("omt_snapshot").await else {
        return;
    };
    let table = |name: &str| ObjectRef::new(ObjectKind::Table, ["public", name]);
    let mut s = db.session().await;
    s.inspect(&Scope::default()).await.unwrap();
    assert!(autocommit(&s).await, "inspection left a transaction open");
    s.execute("CREATE TABLE public.a (id int)").await.unwrap();
    assert!(db.inspect(&Scope::default()).await.contains(&table("a")));

    s.begin().await.unwrap();
    s.execute("CREATE TABLE public.b (id int)").await.unwrap();
    let inside = s.inspect(&Scope::default()).await.unwrap();
    assert!(inside.contains(&table("b")), "uncommitted DDL is visible");
    assert!(
        !autocommit(&s).await,
        "the caller's transaction is still open"
    );
    assert!(!db.inspect(&Scope::default()).await.contains(&table("b")));
    s.rollback().await.unwrap();
    assert!(
        !s.inspect(&Scope::default())
            .await
            .unwrap()
            .contains(&table("b"))
    );
    drop(s);
    db.drop().await;
}

fn data_table(schema: &str, table: &str) -> openmig_core::DataTable {
    openmig_core::DataTable {
        path: vec![schema.into(), table.into()],
        columns: Vec::new(),
        mode: None,
    }
}
