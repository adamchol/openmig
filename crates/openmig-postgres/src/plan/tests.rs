//! Planning synthetic models, without a database.

use crate::PostgresEngine;
use crate::model::{
    Column, Database, Dependency, ForeignKey, Index, IndexKey, PrimaryKey, QualifiedName, Schema,
    Table, View, ViewColumn,
};
use crate::settings::PgSettings;
use openmig_core::{
    DiffContext, Engine, Error, ObjectKind, ObjectRef, Plan, PlanMode, RenameSet, Scope,
};

fn plan_with(current: &Database, desired: &Database, mode: PlanMode) -> Result<Plan, Error> {
    let settings = PgSettings::default();
    let scope = Scope::default();
    let renames = RenameSet::default();
    let ctx = DiffContext::<PostgresEngine> {
        settings: &settings,
        scope: &scope,
        renames: &renames,
        mode,
        allow_recreate: false,
    };
    let changes = PostgresEngine::diff(current, desired, &ctx)?;
    PostgresEngine::plan(changes, current, desired, &ctx)
}

fn sql(plan: &Plan) -> Vec<String> {
    plan.statements.iter().map(|s| s.sql.clone()).collect()
}

fn position(plan: &[String], needle: &str) -> usize {
    plan.iter()
        .position(|s| s.contains(needle))
        .unwrap_or_else(|| panic!("no statement contains `{needle}`:\n{}", plan.join("\n")))
}

fn db(tables: Vec<Table>) -> Database {
    let mut schema = Schema::new("app");
    for t in tables {
        schema.tables.insert(t.name.clone(), t);
    }
    let mut db = Database {
        server_major: 17,
        ..Database::default()
    };
    db.schemas.insert("app".into(), schema);
    db
}

fn table(name: &str, columns: &[(&str, &str)]) -> Table {
    let mut t = Table::new(name);
    t.columns = columns.iter().map(|(n, ty)| Column::new(*n, *ty)).collect();
    t
}

fn with_pk(mut t: Table) -> Table {
    t.primary_key = Some(PrimaryKey {
        name: format!("{}_pkey", t.name),
        columns: vec!["id".into()],
        ..PrimaryKey::default()
    });
    t
}

fn fk(name: &str, column: &str, target: &str) -> ForeignKey {
    ForeignKey {
        name: name.into(),
        columns: vec![column.into()],
        ref_table: QualifiedName::new("app", target),
        ref_columns: vec!["id".into()],
        on_update: Default::default(),
        on_delete: Default::default(),
        match_type: Default::default(),
        deferrable: false,
        initially_deferred: false,
        validated: true,
        enforced: true,
        comment: None,
    }
}

fn r(kind: ObjectKind, path: &[&str]) -> ObjectRef {
    ObjectRef::new(kind, path.iter().copied())
}

#[test]
fn foreign_key_cycles_are_added_after_the_tables() {
    let mut a = with_pk(table("a", &[("id", "integer"), ("b_id", "integer")]));
    let mut b = with_pk(table("b", &[("id", "integer"), ("a_id", "integer")]));
    let mut c = with_pk(table("c", &[("id", "integer"), ("a_id", "integer")]));
    a.foreign_keys.insert("a_b".into(), fk("a_b", "b_id", "b"));
    b.foreign_keys.insert("b_a".into(), fk("b_a", "a_id", "a"));
    c.foreign_keys.insert("c_a".into(), fk("c_a", "a_id", "a"));
    let desired = db(vec![a, b, c]);
    let plan = sql(&plan_with(&db(vec![]), &desired, PlanMode::Generate).unwrap());
    assert_eq!(plan.len(), 5, "{plan:#?}");
    assert!(plan[2].starts_with("CREATE TABLE \"app\".\"c\"") && plan[2].contains("\"c_a\""));
    let ta = position(&plan, "CREATE TABLE \"app\".\"a\"");
    let tb = position(&plan, "CREATE TABLE \"app\".\"b\"");
    let fa = position(&plan, "ADD CONSTRAINT \"a_b\"");
    let fb = position(&plan, "ADD CONSTRAINT \"b_a\"");
    assert!(ta < fa && tb < fa && ta < fb && tb < fb, "{plan:#?}");

    // Dropping the tables drops the cycle's constraints first. Inspection records what the
    // foreign keys depend on.
    let mut current = desired.clone();
    for (from_table, name, target) in [("a", "a_b", "b"), ("b", "b_a", "a"), ("c", "c_a", "a")] {
        current.dependencies.push(Dependency {
            from: r(ObjectKind::ForeignKey, &["app", from_table, name]),
            to: r(
                ObjectKind::PrimaryKey,
                &["app", target, &format!("{target}_pkey")],
            ),
        });
    }
    current.normalize_dependencies();
    let plan = sql(&plan_with(&current, &db(vec![]), PlanMode::Generate).unwrap());
    let da = position(&plan, "DROP TABLE \"app\".\"a\"");
    let dc = position(&plan, "DROP TABLE \"app\".\"c\"");
    assert!(position(&plan, "DROP CONSTRAINT \"a_b\"") < da, "{plan:#?}");
    assert!(dc < da, "the referencing table goes first: {plan:#?}");
}

#[test]
fn views_on_a_retyped_column_are_rebuilt() {
    let t = table("t", &[("id", "integer"), ("n", "integer")]);
    let view = |ty: &str| View {
        name: "v".into(),
        query: " SELECT n FROM app.t;".into(),
        columns: vec![ViewColumn {
            name: "n".into(),
            data_type: ty.into(),
            ..ViewColumn::default()
        }],
        ..View::default()
    };
    let mut current = db(vec![t.clone()]);
    current
        .schemas
        .get_mut("app")
        .unwrap()
        .views
        .insert("v".into(), view("integer"));
    current.dependencies.push(Dependency {
        from: r(ObjectKind::View, &["app", "v"]),
        to: r(ObjectKind::Column, &["app", "t", "n"]),
    });
    let mut retyped = t;
    retyped.columns[1].data_type = "bigint".into();
    let mut desired = db(vec![retyped]);
    desired
        .schemas
        .get_mut("app")
        .unwrap()
        .views
        .insert("v".into(), view("bigint"));
    let plan = sql(&plan_with(&current, &desired, PlanMode::Generate).unwrap());
    assert_eq!(
        plan,
        vec![
            "DROP VIEW \"app\".\"v\"".to_owned(),
            "ALTER TABLE \"app\".\"t\" ALTER COLUMN \"n\" TYPE bigint USING \"n\"::bigint"
                .to_owned(),
            "CREATE VIEW \"app\".\"v\" AS SELECT n FROM app.t".to_owned(),
        ]
    );
}

#[test]
fn dependants_are_dropped_first() {
    let t = with_pk(table("t", &[("id", "integer")]));
    let mut current = db(vec![t]);
    let schema = current.schemas.get_mut("app").unwrap();
    schema.views.insert(
        "v".into(),
        View {
            name: "v".into(),
            query: "SELECT id FROM app.t".into(),
            ..View::default()
        },
    );
    current.dependencies.push(Dependency {
        from: r(ObjectKind::View, &["app", "v"]),
        to: r(ObjectKind::Column, &["app", "t", "id"]),
    });
    let mut desired = Database {
        server_major: 17,
        ..Database::default()
    };
    desired.schemas.insert("app".into(), Schema::new("app"));
    let plan = sql(&plan_with(&current, &desired, PlanMode::Generate).unwrap());
    assert_eq!(
        plan,
        vec![
            "DROP VIEW \"app\".\"v\"".to_owned(),
            "DROP TABLE \"app\".\"t\"".to_owned()
        ]
    );
    let statements = plan_with(&current, &desired, PlanMode::Generate).unwrap();
    assert!(statements.statements[1].destructive.is_some());
}

#[test]
fn a_name_is_freed_before_it_is_taken() {
    let mut cur = table("t", &[("id", "integer"), ("k", "integer")]);
    cur.indexes.insert(
        "t_k_key".into(),
        Index {
            name: "t_k_key".into(),
            keys: vec![IndexKey::column("k")],
            ..Index::default()
        },
    );
    let mut des = table("t", &[("id", "integer"), ("k", "integer")]);
    des.uniques.insert(
        "t_k_key".into(),
        crate::model::Unique {
            name: "t_k_key".into(),
            columns: vec!["k".into()],
            include: vec![],
            nulls_distinct: true,
            deferrable: false,
            initially_deferred: false,
            storage_params: Default::default(),
            comment: None,
        },
    );
    let plan = sql(&plan_with(&db(vec![cur]), &db(vec![des]), PlanMode::Generate).unwrap());
    assert_eq!(
        plan,
        vec![
            "DROP INDEX \"app\".\"t_k_key\"".to_owned(),
            "ALTER TABLE \"app\".\"t\" ADD CONSTRAINT \"t_k_key\" UNIQUE (\"k\")".to_owned(),
        ]
    );
}

#[test]
fn scratch_builds_never_drop_and_create_roles() {
    let mut desired = db(vec![table("t", &[("id", "integer")])]);
    desired
        .roles
        .insert("web".into(), crate::model::Role::new("web"));
    desired.external_roles.insert("ext".into());
    let mut current = Database {
        server_major: 17,
        ..Database::default()
    };
    current
        .schemas
        .insert("public".into(), Schema::new("public"));
    let plan = sql(&plan_with(&current, &desired, PlanMode::Scratch).unwrap());
    assert!(plan[0].starts_with("DO $$BEGIN IF NOT EXISTS"), "{plan:#?}");
    assert_eq!(plan[1], "CREATE ROLE \"web\"");
    assert!(!plan.iter().any(|s| s.starts_with("DROP")), "{plan:#?}");
}

#[test]
fn plans_are_deterministic() {
    let mut a = with_pk(table("a", &[("id", "integer"), ("b_id", "integer")]));
    a.foreign_keys.insert("a_b".into(), fk("a_b", "b_id", "b"));
    let b = with_pk(table("b", &[("id", "integer")]));
    let desired = db(vec![a, b]);
    let first = plan_with(&db(vec![]), &desired, PlanMode::Generate).unwrap();
    for _ in 0..5 {
        assert_eq!(
            plan_with(&db(vec![]), &desired, PlanMode::Generate).unwrap(),
            first
        );
    }
    let plan = sql(&first);
    assert!(position(&plan, "TABLE \"app\".\"b\"") < position(&plan, "TABLE \"app\".\"a\""));
}

#[test]
fn scratch_builds_order_by_identifiers_in_sql() {
    use crate::model::{Function, FunctionBody, Parallel, Returns, Volatility};
    let function = |name: &str, body: &str| Function {
        name: name.into(),
        args: vec![],
        returns: Some(Returns::Type {
            data_type: "integer".into(),
            setof: false,
        }),
        language: "sql".into(),
        body: FunctionBody::Definition(body.into()),
        volatility: Volatility::Volatile,
        strict: false,
        security_definer: false,
        leakproof: false,
        parallel: Parallel::Unsafe,
        cost: "100".into(),
        rows: "0".into(),
        window: false,
        config: Default::default(),
        comment: None,
        owner: String::new(),
        acl: Default::default(),
    };
    let mut items = table("items", &[("code", "integer")]);
    items.columns[0].default = Some("app.next_code()".into());
    let mut desired = db(vec![items]);
    let schema = desired.schemas.get_mut("app").unwrap();
    // The body reads the table, the table's default calls the function: the default wins.
    schema.functions.insert(
        "next_code()".into(),
        function("next_code", "SELECT max(code) + 1 FROM app.items"),
    );
    schema.views.insert(
        "v".into(),
        View {
            name: "v".into(),
            query: "SELECT code FROM items".into(),
            ..View::default()
        },
    );
    let mut current = Database {
        server_major: 17,
        ..Database::default()
    };
    current
        .schemas
        .insert("public".into(), Schema::new("public"));
    let plan = sql(&plan_with(&current, &desired, PlanMode::Scratch).unwrap());
    let f = position(&plan, "CREATE FUNCTION \"app\".\"next_code\"");
    let t = position(&plan, "CREATE TABLE \"app\".\"items\"");
    assert!(
        plan[0].starts_with("SET check_function_bodies = off"),
        "{plan:#?}"
    );
    assert!(f < t, "{plan:#?}");
    // Unqualified names resolve through the search path, which does not include `app`.
    assert!(position(&plan, "CREATE VIEW \"app\".\"v\"") > 0);
}
