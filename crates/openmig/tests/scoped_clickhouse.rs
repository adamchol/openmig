//! OpenMig living inside a ClickHouse server it doesn't own: scoped to the `app` database, with
//! foreign databases, users, roles, a row policy and a SQL function already on the server
//! (`tests/scoped/clickhouse.sql`). Nothing outside the scope may be planned, changed or reported
//! as drift.

use clap::Parser;
use openmig::cli::Cli;
use openmig::term::RecordingUi;
use openmig_core::Error;
use openmig_testkit::ChLease;
use std::path::Path;

const FOREIGN: &str = include_str!("scoped/clickhouse.sql");

/// Some tests lease two servers; running one at a time keeps them from each holding one and
/// waiting for the pool's last.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("db/schema")).unwrap();
        std::fs::write(
            dir.path().join("openmig.toml"),
            format!(
                "[databases.app]\nengine = \"clickhouse\"\nschema = [\"db/schema\"]\n\
                 migrations = \"db/migrations\"\nurl = \"{url}\"\nschemas = [\"app\"]\n\
                 default_database = \"app\"\nhistory_table = \"app_migrations.history\"\n"
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn schema(&self, hcl: &str) {
        std::fs::write(self.path().join("db/schema/app.hcl"), hcl).unwrap();
    }

    fn migrations_dir(&self) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(self.path().join("db/migrations"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn migrations(&self) -> String {
        self.migrations_dir()
            .iter()
            .filter(|name| name.ends_with(".sql"))
            .map(|name| std::fs::read_to_string(self.path().join("db/migrations").join(name)))
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
    }

    async fn run(&self, args: &[&str]) -> (Result<u8, Error>, String) {
        let config = self.path().join("openmig.toml");
        let mut argv = vec!["openmig", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).unwrap();
        let ui = RecordingUi::default();
        let result = openmig::flows::run(cli, &ui).await;
        let messages = ui.messages.lock().unwrap().join("\n");
        (result, messages)
    }

    async fn ok(&self, args: &[&str]) -> String {
        match self.run(args).await {
            (Ok(0), out) => out,
            (result, out) => panic!("openmig {args:?}: {result:?}\n{out}"),
        }
    }
}

async fn query(lease: &ChLease, sql: &str) -> String {
    lease
        .query(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// A server holding the foreign objects of `tests/scoped/clickhouse.sql`.
async fn host() -> ChLease {
    let lease = openmig_testkit::clickhouse().await;
    for statement in FOREIGN.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        query(&lease, statement).await;
    }
    lease
}

/// Everything on the server that OpenMig doesn't own, in a comparable form.
const FOREIGN_STATE: &str = "
SELECT * FROM (
  SELECT 'database' AS k, name AS n, engine AS a, '' AS b FROM system.databases
  WHERE name NOT IN ('app', 'app_migrations')
  UNION ALL
  SELECT 'table', database || '.' || name, engine, '' FROM system.tables
  WHERE database NOT IN ('app', 'app_migrations', 'system', 'INFORMATION_SCHEMA', 'information_schema')
  UNION ALL
  SELECT 'user', name, storage, '' FROM system.users WHERE name NOT IN ('app_svc', 'kafka_sink')
  UNION ALL
  SELECT 'role', name, storage, '' FROM system.roles WHERE name NOT IN ('app_reader')
  UNION ALL
  SELECT 'row_policy', name, ifNull(select_filter, ''), arrayStringConcat(apply_to_list, ',')
  FROM system.row_policies
  UNION ALL
  SELECT 'function', name, create_query, '' FROM system.functions WHERE origin = 'SQLUserDefined'
  UNION ALL
  SELECT 'grant', ifNull(user_name, '') || ifNull(role_name, ''), toString(access_type),
         ifNull(database, '*') || '.' || ifNull(table, '*')
  FROM system.grants
  WHERE ifNull(user_name, '') NOT IN ('app_svc', 'kafka_sink')
    AND ifNull(role_name, '') NOT IN ('app_reader')
  UNION ALL
  SELECT 'role_grant', ifNull(user_name, '') || ifNull(role_name, ''), granted_role_name, ''
  FROM system.role_grants
  WHERE ifNull(user_name, '') NOT IN ('app_svc', 'kafka_sink')
    AND ifNull(role_name, '') NOT IN ('app_reader')
) ORDER BY ALL
FORMAT TSV";

const APP: &str = r#"
database "app" {}
role "app_reader" {}
table "app.session_events" {
  engine   = MergeTree
  column "session_id" { type = UUID }
  column "offset"     { type = UInt64 }
  column "partition"  { type = UInt32 }
  column "ts"         { type = DateTime64(3) }
  column "id"         { type = UUID }
  column "author"     { type = LowCardinality(String) }
  column "kind"       { type = LowCardinality(String) }
  column "data"       { type = String }
  order_by = [column.session_id, column.offset]
  ttl      = "toDateTime(ts) + toIntervalDay(1) WHERE kind = 'model_delta'"
}
table "app.sink_state" {
  engine      = KeeperMap("/app-sink/session_events")
  column "key"       { type = String }
  column "minOffset" { type = Int64 }
  column "maxOffset" { type = Int64 }
  column "state"     { type = String }
  primary_key = [column.key]
}
grant {
  privileges = [SELECT]
  on         = ["app.*"]
  to         = [role.app_reader]
}
"#;

/// `APP` with the project's users: a service that reads through `app_reader`, created with a
/// password hash (of `secret`), and a Kafka Connect sink that the host creates itself.
const APP_USERS: &str = r#"
user "app_svc" {
  auth          = sha256_hash("2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b")
  roles         = [role.app_reader]
  default_roles = [role.app_reader]
}
user "kafka_sink" {}
grant {
  privileges = [INSERT, SELECT]
  on         = ["app.session_events", "app.sink_state"]
  to         = [user.kafka_sink]
}
"#;

fn assert_untouched(plan: &str) {
    for foreign in [
        "their_",
        "analytics",
        "`default`",
        "DROP FUNCTION",
        "DROP ROLE",
        "DROP USER",
        "REVOKE",
    ] {
        assert!(
            !plan.contains(foreign),
            "the plan touches `{foreign}`:\n{plan}"
        );
    }
}

/// Observations 2 and 3: the plan dropped the host's SQL function, and declared roles never made
/// it into migrations, so `generate`, `deploy` and `status --drift` failed with `UNKNOWN_ROLE`.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_generate_deploy_and_drift_leave_the_host_alone() {
    let _serial = SERIAL.lock().await;
    let lease = host().await;
    let project = Project::new(lease.url());
    project.schema(APP);

    let out = project.ok(&["sync", "--dry-run"]).await;
    assert!(out.contains("`app`.`sink_state`"), "{out}");
    assert_untouched(&out);

    project.ok(&["generate", "init"]).await;
    let migrations = project.migrations();
    assert!(
        migrations.contains("CREATE ROLE IF NOT EXISTS `app_reader`"),
        "{migrations}"
    );
    assert_untouched(&migrations);
    project.ok(&["check"]).await;

    let before = query(&lease, FOREIGN_STATE).await;
    project.ok(&["deploy"]).await;
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("1 applied, 0 pending"), "{out}");
    assert!(out.contains("no drift"), "{out}");
    let out = project.ok(&["sync", "--dry-run"]).await;
    assert!(out.contains("up to date"), "{out}");
    pretty_assertions::assert_eq!(before, query(&lease, FOREIGN_STATE).await);

    // The same migrations on a server where the role doesn't exist yet.
    let fresh = openmig_testkit::clickhouse().await;
    project.ok(&["--url", fresh.url(), "deploy"]).await;
    let out = project
        .ok(&["--url", fresh.url(), "status", "--drift"])
        .await;
    assert!(out.contains("no drift"), "{out}");
}

/// The project's ClickHouse access: a user it creates and a user the host provides, with grants.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_users_and_their_grants() {
    let _serial = SERIAL.lock().await;
    let lease = host().await;
    query(
        &lease,
        "CREATE USER kafka_sink IDENTIFIED WITH plaintext_password BY 'x'",
    )
    .await;
    let project = Project::new(lease.url());
    project.schema(&format!("{APP}{APP_USERS}"));

    project.ok(&["generate", "init"]).await;
    let migrations = project.migrations();
    assert!(
        migrations.contains("CREATE USER IF NOT EXISTS `app_svc`"),
        "{migrations}"
    );
    assert!(
        !migrations.contains("CREATE USER IF NOT EXISTS `kafka_sink`"),
        "{migrations}"
    );
    assert_untouched(&migrations);

    let before = query(&lease, FOREIGN_STATE).await;
    project.ok(&["deploy"]).await;
    let grants = query(
        &lease,
        "SELECT ifNull(user_name, role_name), access_type, database, table FROM system.grants \
         WHERE ifNull(user_name, role_name) IN ('app_reader', 'app_svc', 'kafka_sink') \
         ORDER BY ALL FORMAT TSV",
    )
    .await;
    assert_eq!(
        grants,
        "app_reader\tSELECT\tapp\t\\N\n\
         kafka_sink\tSELECT\tapp\tsession_events\n\
         kafka_sink\tSELECT\tapp\tsink_state\n\
         kafka_sink\tINSERT\tapp\tsession_events\n\
         kafka_sink\tINSERT\tapp\tsink_state\n"
    );
    let roles = query(
        &lease,
        "SELECT user_name, granted_role_name, granted_role_is_default FROM system.role_grants \
         WHERE user_name = 'app_svc' FORMAT TSV",
    )
    .await;
    assert_eq!(roles, "app_svc\tapp_reader\t1\n");
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("no drift"), "{out}");
    pretty_assertions::assert_eq!(before, query(&lease, FOREIGN_STATE).await);

    // A user declared without `auth` is never created: deploying where it is missing is an error.
    let fresh = openmig_testkit::clickhouse().await;
    let (result, out) = project.run(&["--url", fresh.url(), "deploy"]).await;
    assert!(
        format!("{result:?}{out}").contains("kafka_sink"),
        "{result:?}\n{out}"
    );
}

/// Observation 5: a migration that fails `generate`'s own verification must not stay behind (the
/// report's case, which verifies once roles are managed).
#[tokio::test(flavor = "multi_thread")]
async fn failed_generate_leaves_the_migrations_unchanged() {
    let _serial = SERIAL.lock().await;
    let lease = host().await;
    let project = Project::new(lease.url());
    project.schema(APP);
    let (result, out) = project.run(&["generate", "init"]).await;
    assert!(
        result.is_ok() || project.migrations_dir().is_empty(),
        "{result:?}\n{out}\n{:?}",
        project.migrations_dir()
    );
}
