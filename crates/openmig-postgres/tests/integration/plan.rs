//! Plan convergence between pairs of schemas built from SQL fixtures (`plan/*.sql`).
//!
//! For a fixture with states A and B: build both in fresh databases, inspect them, plan A -> B
//! (`PlanMode::Generate`), write the plan as migration text, split it back into statements and
//! apply it to A (in one transaction unless the plan forbids it), inspect A again and require
//! that planning A -> B is now empty and that A equals B on every compared field (rows of
//! insert-only data tables may be extra). Then the same from B back to the original A.
//!
//! Fixture format: `-- @both`, `-- @a` and `-- @b` start the SQL sections (`@both` is prepended
//! to the other two). Lines before the first section are options:
//! * `-- concurrent_indexes`, `-- recreate` (`allow_recreate`), `-- no_reverse`;
//! * `-- min_major: 17`: skip on older servers;
//! * `-- schemas: app, other`: schema filter;
//! * `-- roles: omp_a, omp_b`: roles created on the server and declared in the scope;
//! * `-- rename: table app.a app.b` / `-- rename: column app.t.a app.t.b`;
//! * `-- data: app.t sync|insert [columns...]`: data-managed table;
//! * `-- expect: <sql>`: the forward plan contains this text (also `-- expect_reverse:`);
//! * `-- reject: <sql>`: the forward plan does not contain this text;
//! * `-- no_transaction`: the forward plan must run outside a transaction;
//! * `-- unsupported`: the forward diff must fail with an unsupported change.
//!
//! `OPENMIG_SHOW_PLANS=1` prints every plan.

use crate::common::{ROLES, TestDb, admin, run};
use openmig_core::sql::{Dialect, split};
use openmig_core::{
    DataSync, DataTable, DiffContext, Engine, Error, ExcludeSet, ObjectKind, ObjectRef, Plan,
    PlanMode, RenameSet, SchemaFilter, Scope, Session,
};
use openmig_postgres::model::{Database, Schema};
use openmig_postgres::restrict::is_managed_grantee;
use openmig_postgres::{PgSettings, PostgresEngine};
use pretty_assertions::assert_eq;

#[derive(Default)]
struct Fixture {
    settings: PgSettings,
    scope: Scope,
    allow_recreate: bool,
    reverse: bool,
    min_major: u32,
    renames: Vec<(ObjectRef, ObjectRef)>,
    roles: Vec<String>,
    expect: Vec<String>,
    expect_reverse: Vec<String>,
    reject: Vec<String>,
    no_transaction: bool,
    unsupported: bool,
    a: String,
    b: String,
}

fn object(kind: &str, dotted: &str) -> ObjectRef {
    let kind = ObjectKind::parse(kind).unwrap_or_else(|| panic!("unknown kind {kind}"));
    ObjectRef::new(kind, dotted.split('.'))
}

fn parse(text: &str) -> Fixture {
    let mut f = Fixture {
        reverse: true,
        ..Fixture::default()
    };
    let mut section: Option<&str> = None;
    let (mut both, mut a, mut b) = (String::new(), String::new(), String::new());
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(s) = trimmed.strip_prefix("-- @") {
            section = Some(match s.trim() {
                "both" => "both",
                "a" => "a",
                "b" => "b",
                other => panic!("unknown section {other}"),
            });
            continue;
        }
        match section {
            Some("both") => {
                both.push_str(line);
                both.push('\n');
            }
            Some("a") => {
                a.push_str(line);
                a.push('\n');
            }
            Some("b") => {
                b.push_str(line);
                b.push('\n');
            }
            _ => {
                let Some(option) = trimmed.strip_prefix("--") else {
                    continue;
                };
                let (key, value) = option
                    .trim()
                    .split_once(':')
                    .map_or((option.trim(), ""), |(k, v)| (k.trim(), v.trim()));
                match key {
                    "concurrent_indexes" => f.settings.concurrent_indexes = true,
                    "recreate" => f.allow_recreate = true,
                    "no_reverse" => f.reverse = false,
                    "no_transaction" => f.no_transaction = true,
                    "unsupported" => f.unsupported = true,
                    "min_major" => f.min_major = value.parse().unwrap(),
                    "schemas" => {
                        f.scope.schemas = SchemaFilter::Only(
                            value.split(',').map(|s| s.trim().to_owned()).collect(),
                        );
                    }
                    "exclude" => {
                        f.scope.exclude = ExcludeSet::new(value.split(',').map(str::trim)).unwrap();
                    }
                    "roles" => {
                        f.roles = value.split(',').map(|s| s.trim().to_owned()).collect();
                    }
                    "rename" => {
                        let parts: Vec<&str> = value.split_whitespace().collect();
                        let [kind, from, to] = parts.as_slice() else {
                            panic!("rename: <kind> <from> <to>");
                        };
                        f.renames.push((object(kind, from), object(kind, to)));
                    }
                    "data" => {
                        let parts: Vec<&str> = value.split_whitespace().collect();
                        let mode = match parts.get(1).copied() {
                            Some("insert") => DataSync::Insert,
                            _ => DataSync::Sync,
                        };
                        f.scope.data_tables.push(DataTable {
                            path: parts[0].split('.').map(str::to_owned).collect(),
                            columns: parts.iter().skip(2).map(|c| (*c).to_owned()).collect(),
                            mode: Some(mode),
                        });
                        f.scope.data_max_rows = 1000;
                    }
                    "expect" => f.expect.push(value.to_owned()),
                    "expect_reverse" => f.expect_reverse.push(value.to_owned()),
                    "reject" => f.reject.push(value.to_owned()),
                    other if !other.chars().all(|c| c.is_ascii_lowercase() || c == '_') => {}
                    other => panic!("unknown option {other}"),
                }
            }
        }
    }
    f.scope.declared_roles.clone_from(&f.roles);
    f.a = format!("{both}{a}");
    f.b = format!("{both}{b}");
    f
}

pub fn diff_and_plan(
    current: &Database,
    desired: &Database,
    scope: &Scope,
    settings: &PgSettings,
    renames: &RenameSet,
    mode: PlanMode,
    allow_recreate: bool,
) -> Result<Plan, Error> {
    let ctx = DiffContext::<PostgresEngine> {
        settings,
        scope,
        renames,
        mode,
        allow_recreate,
    };
    let changes = PostgresEngine::diff(current, desired, &ctx)?;
    PostgresEngine::plan(changes, current, desired, &ctx)
}

/// Run a plan the way a generated migration file runs: written as text, split again, in one
/// transaction unless a statement forbids it.
pub async fn apply(db: &TestDb, plan: &Plan) -> Result<(), String> {
    let text = plan.to_sql();
    let statements = split(&text, Dialect::Postgres).map_err(|e| e.to_string())?;
    if statements.len() != plan.len() {
        return Err(format!(
            "the plan has {} statements but its text splits into {}:\n{text}",
            plan.len(),
            statements.len()
        ));
    }
    let mut session = db.session().await;
    let transactional = !plan.requires_no_transaction();
    if transactional {
        session.begin().await.map_err(|e| e.to_string())?;
    }
    for s in &statements {
        if let Err(e) = session.execute(&s.sql).await {
            if transactional {
                let _ = session.rollback().await;
            }
            return Err(format!("{e}\nstatement: {}\nplan:\n{text}", s.sql));
        }
    }
    if transactional {
        session.commit().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A model reduced to what the diff compares: no owners, dependencies, versions or column
/// order, and only managed grantees.
pub fn comparable(db: &Database, scope: &Scope) -> Database {
    let mut db = db.clone();
    db.dependencies.clear();
    db.server_major = 0;
    for e in db.extensions.values_mut() {
        e.version.clear();
    }
    for t in db.event_triggers.values_mut() {
        t.owner.clear();
    }
    for r in db.roles.values_mut() {
        if r.valid_until.as_deref() == Some("infinity") {
            r.valid_until = None;
        }
    }
    openmig_postgres::restrict::for_each_acl(&mut db, &mut |acl| {
        acl.retain(|i| is_managed_grantee(&i.grantee, scope));
    });
    for s in db.schemas.values_mut() {
        clear_owners(s);
    }
    db
}

fn clear_owners(s: &mut Schema) {
    s.owner.clear();
    for t in s.tables.values_mut() {
        t.owner.clear();
        t.columns.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for v in s.views.values_mut() {
        v.owner.clear();
    }
    for m in s.materialized_views.values_mut() {
        m.owner.clear();
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

fn reversed(renames: &[(ObjectRef, ObjectRef)]) -> RenameSet {
    let mut set = RenameSet::default();
    for (from, to) in renames.iter().rev() {
        set.push(to.clone(), from.clone());
    }
    set
}

async fn ensure_roles(roles: &[String]) {
    if roles.is_empty() {
        return;
    }
    let mut admin = admin().await;
    for r in roles {
        run(
            &mut admin,
            &format!(
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{r}') THEN \
                 CREATE ROLE \"{r}\" NOLOGIN; END IF; END $$"
            ),
        )
        .await;
    }
}

/// Plan `from` -> `to`, apply it to `db` (which holds `from`) and check convergence.
#[allow(clippy::too_many_arguments)]
async fn converge(
    name: &str,
    db: &TestDb,
    from: &Database,
    to: &Database,
    f: &Fixture,
    renames: &RenameSet,
    expect: &[String],
    reject: &[String],
) -> Plan {
    let plan = diff_and_plan(
        from,
        to,
        &f.scope,
        &f.settings,
        renames,
        PlanMode::Generate,
        f.allow_recreate,
    )
    .unwrap_or_else(|e| panic!("{name}: planning failed: {e}"));
    let text = plan.to_sql();
    if std::env::var_os("OPENMIG_SHOW_PLANS").is_some() {
        eprintln!("--- {name}\n{text}");
    }
    for e in expect {
        assert!(
            text.contains(e.as_str()),
            "{name}: the plan lacks `{e}`:\n{text}"
        );
    }
    for r in reject {
        assert!(
            !text.contains(r.as_str()),
            "{name}: the plan has `{r}`:\n{text}"
        );
    }
    if let Err(e) = apply(db, &plan).await {
        panic!("{name}: applying the plan failed: {e}");
    }
    let after = db.inspect(&f.scope).await;
    let again = diff_and_plan(
        &after,
        to,
        &f.scope,
        &f.settings,
        &RenameSet::default(),
        PlanMode::Generate,
        f.allow_recreate,
    )
    .unwrap_or_else(|e| panic!("{name}: planning again failed: {e}\nfirst plan:\n{text}"));
    assert!(
        again.is_empty(),
        "{name}: does not converge.\nfirst plan:\n{text}\nremaining plan:\n{}",
        again.to_sql()
    );
    let mut after = after;
    for t in &f.scope.data_tables {
        if t.mode == Some(DataSync::Insert) {
            let name = openmig_postgres::model::QualifiedName::new(&t.path[0], &t.path[1]);
            if let (Some(rows), Some(wanted)) = (after.data.get_mut(&name), to.data.get(&name)) {
                rows.rows.retain(|k, _| wanted.rows.contains_key(k));
            }
        }
    }
    assert_eq!(
        comparable(&after, &f.scope),
        comparable(to, &f.scope),
        "{name}: models differ after applying:\n{text}"
    );
    plan
}

async fn run_fixture(name: &str, text: &str) {
    let f = parse(text);
    let _roles = ROLES.read().await;
    let Some(a) = TestDb::create(&format!("omp_{name}_a")).await else {
        return;
    };
    if a.major().await < f.min_major {
        a.drop().await;
        return;
    }
    let b = TestDb::create(&format!("omp_{name}_b")).await.unwrap();
    ensure_roles(&f.roles).await;
    a.run(&f.a).await;
    b.run(&f.b).await;
    let ma = a.inspect(&f.scope).await;
    let mb = b.inspect(&f.scope).await;
    let mut renames = RenameSet::default();
    for (from, to) in &f.renames {
        renames.push(from.clone(), to.clone());
    }
    if f.unsupported {
        let result = diff_and_plan(
            &ma,
            &mb,
            &f.scope,
            &f.settings,
            &renames,
            PlanMode::Generate,
            f.allow_recreate,
        );
        assert!(
            matches!(result, Err(Error::Unsupported { .. })),
            "{name}: expected an unsupported change, got {result:?}"
        );
    } else {
        let plan = converge(name, &a, &ma, &mb, &f, &renames, &f.expect, &f.reject).await;
        assert!(
            !f.no_transaction || plan.requires_no_transaction(),
            "{name}: expected a no-transaction plan:\n{}",
            plan.to_sql()
        );
    }
    if f.reverse {
        converge(
            &format!("{name} (reverse)"),
            &b,
            &mb,
            &ma,
            &f,
            &reversed(&f.renames),
            &f.expect_reverse,
            &[],
        )
        .await;
    }
    a.drop().await;
    b.drop().await;
}

macro_rules! fixtures {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run_fixture(
                    stringify!($name),
                    include_str!(concat!("plan/", stringify!($name), ".sql")),
                )
                .await;
            }
        )*
    };
}

fixtures!(
    tables_and_columns,
    column_type_under_view,
    constraints,
    fk_cycle,
    indexes,
    concurrent_indexes,
    identity,
    generated_columns,
    generated_expression,
    generated_unsupported,
    generated_recreate,
    sequences,
    enums,
    enum_removal,
    enum_recreate_unused,
    views,
    view_columns_change,
    materialized_views,
    functions,
    function_dependants,
    column_type_dependants,
    triggers,
    policies,
    event_triggers,
    grants,
    grants_new_objects,
    comments,
    rename_column,
    rename_table,
    partitions,
    partitioning_unsupported,
    inheritance,
    table_options,
    extensions,
    data,
    schemas,
    composites,
    domains,
    drop_column_under_view,
    primary_key_with_references,
    name_conflicts,
    composite_used_unsupported,
    duplicate_unique,
);

/// Apply a plan built from a decoded schema the way `normalize` does: one transaction with a
/// savepoint per statement; statements failing because an object does not exist yet are retried
/// after the others. Returns how many statements had to be retried.
pub async fn apply_with_retry(
    session: &mut openmig_postgres::PgSession,
    plan: &Plan,
) -> Result<usize, String> {
    let transactional = !plan.requires_no_transaction();
    if transactional {
        session.begin().await.map_err(|e| e.to_string())?;
    }
    let mut queue: Vec<&openmig_core::Statement> = plan.statements.iter().collect();
    let mut retried = 0;
    loop {
        let before = queue.len();
        let mut deferred = Vec::new();
        for stmt in queue {
            let result = if transactional {
                session
                    .execute("SAVEPOINT s")
                    .await
                    .map_err(|e| e.to_string())?;
                match session.execute(&stmt.sql).await {
                    Ok(()) => session
                        .execute("RELEASE SAVEPOINT s")
                        .await
                        .map_err(|e| e.to_string())
                        .map(|()| Ok(()))?,
                    Err(e) => {
                        session
                            .execute("ROLLBACK TO SAVEPOINT s")
                            .await
                            .map_err(|e| e.to_string())?;
                        Err(e)
                    }
                }
            } else {
                session.execute(&stmt.sql).await
            };
            match result {
                Ok(()) => {}
                Err(e) if e.kind == openmig_core::ExecErrorKind::UndefinedObject => {
                    deferred.push((stmt, e));
                }
                Err(e) => {
                    if transactional {
                        let _ = session.rollback().await;
                    }
                    return Err(format!("{e}\nstatement: {}", stmt.sql));
                }
            }
        }
        if deferred.is_empty() {
            break;
        }
        if deferred.len() == before {
            if transactional {
                let _ = session.rollback().await;
            }
            let errors: Vec<String> = deferred
                .iter()
                .map(|(s, e)| format!("{e}\nstatement: {}", s.sql))
                .collect();
            return Err(errors.join("\n"));
        }
        retried += deferred.len();
        queue = deferred.into_iter().map(|(s, _)| s).collect();
    }
    if transactional {
        session.commit().await.map_err(|e| e.to_string())?;
    }
    Ok(retried)
}

const RICH_ROLES: &str = include_str!("sql/rich_roles.sql");
const RICH: &str = include_str!("sql/rich.sql");
const RICH_15: &str = include_str!("sql/rich_15.sql");
const RICH_17: &str = include_str!("sql/rich_17.sql");
const RICH_18: &str = include_str!("sql/rich_18.sql");

fn own_roles(mut model: Database, prefix: &str) -> Database {
    model.roles.retain(|name, _| name.starts_with(prefix));
    model
}

/// The rich fixture, inspected, is planned from a pristine database of another server with
/// `PlanMode::Scratch` as if it had been decoded (no dependencies, one role only referenced),
/// applied with savepoints and retries, and inspected back: the result equals the original.
#[tokio::test]
async fn scratch_build_of_rich_schema() {
    let _roles = ROLES.read().await;
    let prefix = "omp_scr_";
    let Some(source) = TestDb::create("omp_scratch_source").await else {
        return;
    };
    crate::common::drop_roles(prefix).await;
    run(
        &mut admin().await,
        &crate::common::with_prefix(RICH_ROLES, prefix),
    )
    .await;
    let major = source.major().await;
    let mut sql = RICH.to_owned();
    for (min, part) in [(15, RICH_15), (17, RICH_17), (18, RICH_18)] {
        if major >= min {
            sql.push_str(part);
        }
    }
    source.run(&crate::common::with_prefix(&sql, prefix)).await;
    let scope = Scope {
        manage_roles: true,
        data_tables: vec![DataTable {
            path: vec!["app".into(), "statuses".into()],
            columns: Vec::new(),
            mode: Some(DataSync::Sync),
        }],
        data_max_rows: 100,
        ..Scope::default()
    };
    let model = own_roles(source.inspect(&scope).await, prefix);
    let external = format!("{prefix}admin");
    let mut decoded = model.clone();
    decoded.dependencies.clear();
    decoded.server_major = 0;
    decoded.roles.remove(&external);
    decoded.external_roles.insert(external.clone());

    let lease = openmig_testkit::postgres().await;
    let mut scratch = openmig_postgres::PgSession::connect(
        lease.url(),
        &PgSettings::default(),
        openmig_core::SessionPurpose::Scratch,
    )
    .await
    .unwrap();
    let pristine = scratch.inspect(&scope).await.unwrap();
    let plan = diff_and_plan(
        &pristine,
        &decoded,
        &scope,
        &PgSettings::default(),
        &RenameSet::default(),
        PlanMode::Scratch,
        false,
    )
    .unwrap();
    let text = plan.to_sql();
    if std::env::var_os("OPENMIG_SHOW_PLANS").is_some() {
        eprintln!("--- scratch build\n{text}");
    }
    assert!(!text.contains("CONCURRENTLY"));
    let retried = apply_with_retry(&mut scratch, &plan)
        .await
        .unwrap_or_else(|e| panic!("scratch build failed: {e}\n{text}"));
    assert_eq!(retried, 0, "statements had to be retried:\n{text}");
    let mut built = scratch.inspect(&scope).await.unwrap();
    let mut expected = model.clone();
    for m in [&mut built, &mut expected] {
        m.roles.remove(&external);
    }
    assert_eq!(comparable(&built, &scope), comparable(&expected, &scope));
    built
        .roles
        .insert(external.clone(), model.roles[&external].clone());
    let again = diff_and_plan(
        &built,
        &model,
        &scope,
        &PgSettings::default(),
        &RenameSet::default(),
        PlanMode::Generate,
        false,
    )
    .unwrap();
    assert!(
        again
            .statements
            .iter()
            .all(|s| s.sql.starts_with("ALTER ROLE")),
        "only the external role may differ:\n{}",
        again.to_sql()
    );
    drop(scratch);
    drop(lease);
    source.drop().await;
    crate::common::drop_roles(prefix).await;
}

const ROLES_A: &str = "
CREATE ROLE __R__a NOLOGIN;
CREATE ROLE __R__b LOGIN CONNECTION LIMIT 5;
CREATE ROLE __R__gone;
GRANT __R__a TO __R__b;
COMMENT ON ROLE __R__a IS 'a';
";
const SCHEMA_A: &str = "
CREATE SCHEMA app;
CREATE TABLE app.t (id integer, owner text);
GRANT SELECT ON app.t TO __R__a;
GRANT INSERT ON app.t TO __R__gone;
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY pol ON app.t TO __R__gone USING (true);
";
const ROLES_B: &str = "
CREATE ROLE __R__a NOLOGIN CREATEDB;
CREATE ROLE __R__b NOLOGIN NOINHERIT VALID UNTIL '2030-01-01 00:00:00+00';
CREATE ROLE __R__new LOGIN;
GRANT __R__a TO __R__b WITH ADMIN OPTION;
GRANT __R__new TO __R__a;
COMMENT ON ROLE __R__new IS 'new';
";
const SCHEMA_B: &str = "
CREATE SCHEMA app;
CREATE TABLE app.t (id integer, owner text);
GRANT SELECT ON app.t TO __R__new;
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY pol ON app.t TO __R__new, __R__a USING (true);
";

/// Roles are cluster-wide, so A lives on the shared server (only roles with the test's prefix
/// are compared) and B on a server of its own.
#[tokio::test]
async fn roles_and_memberships() {
    let _roles = ROLES.read().await;
    let prefix = "omp_role_";
    let Some(a) = TestDb::create("omp_roles_a").await else {
        return;
    };
    crate::common::drop_roles(prefix).await;
    let lease = openmig_testkit::postgres().await;
    let b = TestDb {
        name: "omp_roles_b".into(),
        url: crate::common::url_for(lease.url(), "omp_roles_b"),
    };
    let mut lease_admin =
        crate::common::connect(lease.url(), openmig_core::SessionPurpose::Target).await;
    run(
        &mut lease_admin,
        "CREATE DATABASE omp_roles_b TEMPLATE template0",
    )
    .await;
    let with = |sql: &str| crate::common::with_prefix(sql, prefix);
    run(&mut admin().await, &with(ROLES_A)).await;
    run(&mut lease_admin, &with(ROLES_B)).await;
    a.run(&with(SCHEMA_A)).await;
    b.run(&with(SCHEMA_B)).await;
    let f = Fixture {
        scope: Scope {
            manage_roles: true,
            schemas: SchemaFilter::Only(vec!["app".into()]),
            ..Scope::default()
        },
        ..Fixture::default()
    };
    let ma = own_roles(a.inspect(&f.scope).await, prefix);
    let mb = own_roles(b.inspect(&f.scope).await, prefix);
    let check = |name: &str, plan: &Plan| {
        let text = plan.to_sql();
        assert!(
            !text.contains("pg_"),
            "{name}: touches other roles:\n{text}"
        );
    };
    for (name, db, from, to) in [("roles", &a, &ma, &mb), ("roles (reverse)", &b, &mb, &ma)] {
        let plan = diff_and_plan(
            from,
            to,
            &f.scope,
            &f.settings,
            &RenameSet::default(),
            PlanMode::Generate,
            false,
        )
        .unwrap();
        check(name, &plan);
        if std::env::var_os("OPENMIG_SHOW_PLANS").is_some() {
            eprintln!("--- {name}\n{}", plan.to_sql());
        }
        apply(db, &plan)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let after = own_roles(db.inspect(&f.scope).await, prefix);
        let again = diff_and_plan(
            &after,
            to,
            &f.scope,
            &f.settings,
            &RenameSet::default(),
            PlanMode::Generate,
            false,
        )
        .unwrap();
        assert!(
            again.is_empty(),
            "{name}: does not converge:\n{}",
            again.to_sql()
        );
        assert_eq!(
            comparable(&after, &f.scope),
            comparable(to, &f.scope),
            "{name}"
        );
    }
    drop(lease_admin);
    drop(lease);
    a.drop().await;
    crate::common::drop_roles(prefix).await;
}

/// Encode a model to HCL and decode it again: the "natural" form a user's schema decodes to.
pub fn decoded_from(model: &Database) -> openmig_core::Desired<Database> {
    let settings = PgSettings::default();
    let files = PostgresEngine::encode(model, &settings, &openmig_core::EncodeOptions::default());
    let spec = PostgresEngine::schema_spec(&settings);
    let sources = files
        .iter()
        .map(|f| (f.path.clone(), f.content.clone()))
        .collect();
    let opts = openmig_hcl::LoadOptions {
        spec: &spec,
        vars: Default::default(),
    };
    let doc = openmig_hcl::load_sources(sources, &opts)
        .unwrap_or_else(|e| panic!("encoded HCL does not load:\n{}", e.render(false)));
    PostgresEngine::decode(&doc, &settings).unwrap_or_else(|e| panic!("decode: {e}"))
}

/// The real normalization path: the rich schema is inspected, encoded to HCL and decoded, then
/// built from a pristine database of another server with `PlanMode::Scratch`. Inspecting the
/// result gives the original schema back.
#[tokio::test]
async fn scratch_build_from_decoded_rich_schema() {
    let _roles = ROLES.read().await;
    let prefix = "omp_dec_";
    let Some(source) = TestDb::create("omp_decoded_source").await else {
        return;
    };
    crate::common::drop_roles(prefix).await;
    run(
        &mut admin().await,
        &crate::common::with_prefix(RICH_ROLES, prefix),
    )
    .await;
    let major = source.major().await;
    let mut sql = RICH.to_owned();
    for (min, part) in [(15, RICH_15), (17, RICH_17), (18, RICH_18)] {
        if major >= min {
            sql.push_str(part);
        }
    }
    source.run(&crate::common::with_prefix(&sql, prefix)).await;
    let mut scope = Scope {
        manage_roles: true,
        data_tables: vec![DataTable {
            path: vec!["app".into(), "statuses".into()],
            columns: Vec::new(),
            mode: Some(DataSync::Sync),
        }],
        data_max_rows: 100,
        ..Scope::default()
    };
    let model = own_roles(source.inspect(&scope).await, prefix);
    let decoded = decoded_from(&model).model;
    scope.declared_roles = PostgresEngine::declared_roles(&decoded);

    let lease = openmig_testkit::postgres().await;
    let mut scratch = openmig_postgres::PgSession::connect(
        lease.url(),
        &PgSettings::default(),
        openmig_core::SessionPurpose::Scratch,
    )
    .await
    .unwrap();
    let pristine = scratch.inspect(&scope).await.unwrap();
    let plan = diff_and_plan(
        &pristine,
        &decoded,
        &scope,
        &PgSettings::default(),
        &RenameSet::default(),
        PlanMode::Scratch,
        false,
    )
    .unwrap();
    let text = plan.to_sql();
    if std::env::var_os("OPENMIG_SHOW_PLANS").is_some() {
        eprintln!("--- decoded scratch build\n{text}");
    }
    let retried = apply_with_retry(&mut scratch, &plan)
        .await
        .unwrap_or_else(|e| panic!("scratch build failed: {e}\n{text}"));
    let built = scratch.inspect(&scope).await.unwrap();
    assert_eq!(comparable(&built, &scope), comparable(&model, &scope));
    assert_eq!(retried, 0, "statements had to be retried:\n{text}");
    drop(scratch);
    drop(lease);
    source.drop().await;
    crate::common::drop_roles(prefix).await;
}
