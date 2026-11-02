//! OpenMig living inside a PostgreSQL server it doesn't own: scoped to the `app` schema, with
//! foreign schemas, roles, grants, an event trigger, an extension, a publication and default
//! privileges already in the database (`tests/scoped/postgres.sql`). Nothing outside the scope may
//! be planned, changed or reported as drift.

use clap::Parser;
use openmig::cli::Cli;
use openmig::term::RecordingUi;
use openmig_core::Error;
use openmig_testkit::PgLease;
use std::path::Path;
use std::process::Command;

const FOREIGN: &str = include_str!("scoped/postgres.sql");

/// Some tests lease two servers; running one at a time keeps them from each holding one and
/// waiting for the pool's last.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(url: &str, options: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("db/schema")).unwrap();
        std::fs::write(
            dir.path().join("openmig.toml"),
            format!(
                "[databases.app]\nengine = \"postgres\"\nschema = [\"db/schema\"]\n\
                 migrations = \"db/migrations\"\nurl = \"{url}\"\nversion = 17\n\
                 schemas = [\"app\"]\nhistory_table = \"app_migrations.history\"\n{options}"
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

    fn migrations_dir(&self) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = std::fs::read_dir(self.path().join("db/migrations"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            std::fs::read_to_string(e.path()).unwrap(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn migrations(&self) -> String {
        self.migrations_dir()
            .into_iter()
            .filter(|(name, _)| name.ends_with(".sql"))
            .map(|(_, content)| content)
            .collect::<Vec<_>>()
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

fn psql(lease: &PgLease, url: &str, sql: &str) -> String {
    let out = Command::new(lease.bin_dir().join("psql"))
        .args(["-X", "-A", "-t", "-q", "-F", "|", "-v", "ON_ERROR_STOP=1"])
        .args(["-d", url, "-c", sql])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "psql: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A database holding the foreign objects of `tests/scoped/postgres.sql`.
async fn host(lease: &PgLease) -> String {
    let url = lease.create_database("host").await;
    psql(lease, &url, FOREIGN);
    url
}

/// Everything in the database and cluster that OpenMig doesn't own, in a comparable form.
const FOREIGN_STATE: &str = "
WITH foreign_ns AS (
  SELECT oid, nspname FROM pg_namespace
  WHERE nspname NOT IN ('app', 'app_migrations', 'information_schema') AND nspname !~ '^pg_'
)
SELECT * FROM (
  SELECT 'schema' AS k, nspname::text AS n,
         coalesce(obj_description(oid, 'pg_namespace'), '') AS a, coalesce(nspacl::text, '') AS b
  FROM pg_namespace WHERE oid IN (SELECT oid FROM foreign_ns)
  UNION ALL
  SELECT 'role', rolname::text, concat_ws(',', rolcanlogin, rolreplication, rolsuper), ''
  FROM pg_roles WHERE rolname !~ '^pg_' AND rolname NOT IN ('cdc_reader', 'app_api')
  UNION ALL
  SELECT 'class', ns.nspname || '.' || c.relname, c.relkind::text, coalesce(c.relacl::text, '')
  FROM pg_class c JOIN foreign_ns ns ON ns.oid = c.relnamespace
  UNION ALL
  SELECT 'proc', ns.nspname || '.' || p.proname, '', coalesce(p.proacl::text, '')
  FROM pg_proc p JOIN foreign_ns ns ON ns.oid = p.pronamespace
  UNION ALL
  SELECT 'event_trigger', evtname::text, evtevent::text, evtenabled::text FROM pg_event_trigger
  UNION ALL
  SELECT 'extension', extname::text, extversion, extnamespace::regnamespace::text FROM pg_extension
  UNION ALL
  SELECT 'publication', pubname::text, concat_ws(',', puballtables, pubinsert, pubupdate, pubdelete),
         coalesce((SELECT string_agg(prrelid::regclass::text, ',' ORDER BY 1)
                   FROM pg_publication_rel r WHERE r.prpubid = p.oid), '')
  FROM pg_publication p WHERE pubname <> 'app_cdc'
  UNION ALL
  SELECT 'default_acl', defaclrole::regrole::text, defaclobjtype::text, defaclacl::text
  FROM pg_default_acl
) s ORDER BY 1, 2, 3, 4";

const APP: &str = r#"
schema "app" {}
role "cdc_reader" {}
table "app.grants" {
  column "subject" {
    type = text
    null = false
  }
  column "session_id" {
    type = uuid
    null = false
  }
  primary_key {
    columns = [column.subject, column.session_id]
  }
}
grant {
  privileges = [SELECT]
  on         = [table.app.grants]
  to         = [role.cdc_reader]
}
"#;

/// `APP` with the roles the project really needs: an API service and a CDC reader.
const APP_ROLES: &str = r#"
schema "app" {}
role "app_api" {
  login = true
}
role "cdc_reader" {
  login       = true
  replication = true
}
table "app.grants" {
  column "subject" {
    type = text
    null = false
  }
  column "session_id" {
    type = uuid
    null = false
  }
  primary_key {
    columns = [column.subject, column.session_id]
  }
}
grant {
  privileges = [SELECT, INSERT, UPDATE, DELETE]
  on         = [table.app.grants]
  to         = [role.app_api]
}
grant {
  privileges = [SELECT]
  on         = [table.app.grants]
  to         = [role.cdc_reader]
}
grant {
  privileges = [USAGE]
  on         = [schema.app]
  to         = [role.app_api, role.cdc_reader]
}
"#;

/// `APP_ROLES` plus the publication the CDC reader streams `app.grants` through.
fn app_cdc(publication_body: &str) -> String {
    format!("{APP_ROLES}\npublication \"app_cdc\" {{\n{publication_body}}}\n")
}

fn assert_untouched(plan: &str) {
    for foreign in [
        "\"public\"",
        "public.",
        "billing",
        "their_",
        "customers",
        "pgcrypto",
        "EVENT TRIGGER",
    ] {
        assert!(
            !plan.contains(foreign),
            "the plan touches `{foreign}`:\n{plan}"
        );
    }
}

/// Observation 1: scoping to `app` made every command fail with `CREATE SCHEMA "public"`.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_does_not_recreate_public() {
    let _serial = SERIAL.lock().await;
    let lease = openmig_testkit::postgres().await;
    let url = host(&lease).await;
    for options in ["default_schema = \"app\"\n", ""] {
        let project = Project::new(&url, options);
        project.schema(APP);
        let out = project.ok(&["sync", "--dry-run"]).await;
        assert!(out.contains("CREATE TABLE \"app\".\"grants\""), "{out}");
        assert_untouched(&out);
        project.ok(&["generate", "init"]).await;
        assert_untouched(&project.migrations());
    }
}

/// Observations 2 and 3: the plan dropped the host's event trigger, and declared roles never made
/// it into migrations, so `generate` failed its own replay and `deploy` worked only where the role
/// already existed.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_generate_deploy_and_drift_leave_the_host_alone() {
    let _serial = SERIAL.lock().await;
    let lease = openmig_testkit::postgres().await;
    let url = host(&lease).await;
    let project = Project::new(&url, "default_schema = \"app\"\n");
    project.schema(APP);

    let out = project.ok(&["sync", "--dry-run"]).await;
    assert_untouched(&out);

    project.ok(&["generate", "init"]).await;
    let migrations = project.migrations();
    assert!(
        migrations.contains("CREATE ROLE \"cdc_reader\""),
        "{migrations}"
    );
    assert_untouched(&migrations);
    project.ok(&["check"]).await;

    let before = psql(&lease, &url, FOREIGN_STATE);
    project.ok(&["deploy"]).await;
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("1 applied, 0 pending"), "{out}");
    assert!(out.contains("no drift"), "{out}");
    let out = project.ok(&["sync", "--dry-run"]).await;
    assert!(out.contains("up to date"), "{out}");
    pretty_assertions::assert_eq!(before, psql(&lease, &url, FOREIGN_STATE));

    // The host's default privileges gave `their_reader` SELECT on the new table: not ours.
    let acl = psql(
        &lease,
        &url,
        "SELECT relacl::text FROM pg_class WHERE oid = 'app.grants'::regclass",
    );
    assert!(acl.contains("their_reader=r/"), "{acl}");
    assert!(acl.contains("cdc_reader=r/"), "{acl}");

    // The same migrations on a server where none of the roles exist yet.
    let fresh = openmig_testkit::postgres().await;
    let fresh_url = fresh.create_database("app").await;
    project.ok(&["--url", &fresh_url, "deploy"]).await;
    let out = project
        .ok(&["--url", &fresh_url, "status", "--drift"])
        .await;
    assert!(out.contains("no drift"), "{out}");
}

/// Observation 3: the project's own roles with their attributes, next to the host's roles.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_roles_with_attributes_and_grants() {
    let _serial = SERIAL.lock().await;
    let lease = openmig_testkit::postgres().await;
    let url = host(&lease).await;
    let project = Project::new(&url, "default_schema = \"app\"\n");
    project.schema(APP_ROLES);

    project.ok(&["generate", "init"]).await;
    let migrations = project.migrations();
    assert!(migrations.contains("\"app_api\""), "{migrations}");
    assert_untouched(&migrations);
    let before = psql(&lease, &url, FOREIGN_STATE);
    project.ok(&["deploy"]).await;
    let roles = psql(
        &lease,
        &url,
        "SELECT rolname, rolcanlogin, rolreplication FROM pg_roles \
         WHERE rolname IN ('app_api', 'cdc_reader') ORDER BY 1",
    );
    assert_eq!(roles, "app_api|t|f\ncdc_reader|t|t\n");
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("no drift"), "{out}");
    pretty_assertions::assert_eq!(before, psql(&lease, &url, FOREIGN_STATE));

    // Someone takes REPLICATION away from the CDC reader: that is drift on our role.
    psql(&lease, &url, "ALTER ROLE cdc_reader NOREPLICATION");
    let (result, out) = project.run(&["status", "--drift"]).await;
    assert!(matches!(result, Err(Error::Drift(_))), "{result:?}\n{out}");
}

/// Observation 4: a publication the CDC reader replicates `app.grants` through.
#[tokio::test(flavor = "multi_thread")]
async fn scoped_publication() {
    let _serial = SERIAL.lock().await;
    let lease = openmig_testkit::postgres().await;
    let url = host(&lease).await;
    let project = Project::new(&url, "default_schema = \"app\"\n");
    project.schema(&app_cdc("  tables = [table.app.grants]\n"));

    let out = project.ok(&["sync", "--dry-run"]).await;
    assert!(out.contains("CREATE PUBLICATION \"app_cdc\""), "{out}");
    assert_untouched(&out);
    project.ok(&["generate", "init"]).await;
    let before = psql(&lease, &url, FOREIGN_STATE);
    project.ok(&["deploy"]).await;
    let tables = psql(
        &lease,
        &url,
        "SELECT pubname, schemaname, tablename FROM pg_publication_tables ORDER BY 1",
    );
    assert_eq!(tables, "app_cdc|app|grants\ntheir_pub|public|customers\n");
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("no drift"), "{out}");
    pretty_assertions::assert_eq!(before, psql(&lease, &url, FOREIGN_STATE));

    project.schema(&app_cdc(
        "  tables  = [table.app.grants]\n  publish = [INSERT, UPDATE, DELETE]\n",
    ));
    project.ok(&["generate", "no_truncate"]).await;
    assert!(
        project
            .migrations()
            .contains("ALTER PUBLICATION \"app_cdc\""),
        "{}",
        project.migrations()
    );
    project.ok(&["deploy"]).await;
    let publish = psql(
        &lease,
        &url,
        "SELECT pubinsert, pubupdate, pubdelete, pubtruncate FROM pg_publication \
         WHERE pubname = 'app_cdc'",
    );
    assert_eq!(publish, "t|t|t|f\n");

    psql(
        &lease,
        &url,
        "ALTER PUBLICATION app_cdc SET (publish = 'insert')",
    );
    let (result, out) = project.run(&["status", "--drift"]).await;
    assert!(matches!(result, Err(Error::Drift(_))), "{result:?}\n{out}");
    pretty_assertions::assert_eq!(before, psql(&lease, &url, FOREIGN_STATE));
}

/// Observation 5: a migration that fails `generate`'s own verification must not stay behind. This
/// is the report's case, which verifies once roles are managed; the cleanup itself is covered by
/// the migrate crate's tests.
#[tokio::test(flavor = "multi_thread")]
async fn failed_generate_leaves_the_migrations_unchanged() {
    let _serial = SERIAL.lock().await;
    let lease = openmig_testkit::postgres().await;
    let url = host(&lease).await;
    let project = Project::new(&url, "default_schema = \"app\"\n");
    project.schema(APP);
    let (result, out) = project.run(&["generate", "init"]).await;
    assert!(
        result.is_ok() || project.migrations_dir().is_empty(),
        "{result:?}\n{out}\n{:#?}",
        project.migrations_dir()
    );
}
