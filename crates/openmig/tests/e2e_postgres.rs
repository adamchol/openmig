//! End-to-end runs of the `openmig` command line against a real PostgreSQL server.

use clap::Parser;
use openmig::cli::Cli;
use openmig::term::RecordingUi;
use openmig_core::Error;
use std::path::Path;
use std::process::Command;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("schema")).unwrap();
        std::fs::write(
            dir.path().join("openmig.toml"),
            format!(
                "[databases.app]\nengine = \"postgres\"\nschema = [\"schema\"]\nmigrations = \"migrations\"\nurl = \"{url}\"\nversion = 17\n"
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn schema(&self, hcl: &str) {
        std::fs::write(self.path().join("schema/app.hcl"), hcl).unwrap();
    }

    fn migrations(&self) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(self.path().join("migrations"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".sql"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    async fn run(&self, args: &[&str]) -> (Result<u8, Error>, Vec<String>) {
        let config = self.path().join("openmig.toml");
        let mut argv = vec!["openmig", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).unwrap();
        let ui = RecordingUi::default();
        let result = openmig::flows::run(cli, &ui).await;
        let messages = ui.messages.lock().unwrap().clone();
        (result, messages)
    }

    async fn ok(&self, args: &[&str]) -> Vec<String> {
        let (result, messages) = self.run(args).await;
        match result {
            Ok(0) => messages,
            Ok(code) => panic!(
                "openmig {args:?} exited with {code}\n{}",
                messages.join("\n")
            ),
            Err(e) => panic!("openmig {args:?} failed: {e}\n{}", messages.join("\n")),
        }
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
}

const V1: &str = r#"
schema "app" {}

table "app.accounts" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}
"#;

const V2: &str = r#"
schema "app" {}

table "app.accounts" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  column "created_at" {
    type    = timestamptz
    null    = false
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  index "accounts_email_key" {
    unique = true
    key {
      expr = "lower(email)"
    }
  }
}

view "app.recent_accounts" {
  as = "SELECT id, email FROM app.accounts WHERE created_at > now() - interval '7 days'"
}
"#;

fn joined(messages: &[String]) -> String {
    messages.join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn develop_generate_check_deploy() {
    let lease = openmig_testkit::postgres().await;
    let dev = lease.create_database("e2e_dev").await;
    let prod = lease.create_database("e2e_prod").await;
    let project = Project::new(&dev);

    project.schema(V1);
    let out = project.ok(&["sync", "--yes"]).await;
    assert!(joined(&out).contains("applied"), "{}", joined(&out));
    let out = project.ok(&["sync", "--yes"]).await;
    assert!(joined(&out).contains("up to date"), "{}", joined(&out));

    project.ok(&["generate", "init"]).await;
    assert_eq!(project.migrations().len(), 1);
    assert!(project.migrations()[0].ends_with("_init.sql"));
    project.ok(&["check"]).await;

    project.schema(V2);
    let (result, out) = project.run(&["check"]).await;
    assert!(
        matches!(result, Err(Error::Drift(_))),
        "{result:?}\n{}",
        joined(&out)
    );
    project.ok(&["generate", "add_created_at"]).await;
    assert_eq!(project.migrations().len(), 2);
    let second = std::fs::read_to_string(
        project
            .path()
            .join("migrations")
            .join(&project.migrations()[1]),
    )
    .unwrap();
    assert!(second.starts_with("-- openmig:generated\n"), "{second}");
    assert!(second.contains("ADD COLUMN \"created_at\""), "{second}");
    project.ok(&["check"]).await;
    project.ok(&["generate"]).await;
    assert_eq!(
        project.migrations().len(),
        2,
        "no changes must not write a file"
    );

    project.ok(&["sync", "--yes"]).await;
    let out = project
        .ok(&["diff", "--from", "migrations", "--to", "schema"])
        .await;
    assert!(joined(&out).contains("no differences"), "{}", joined(&out));

    project.ok(&["--url", &prod, "deploy"]).await;
    let out = project.ok(&["--url", &prod, "status", "--drift"]).await;
    assert!(
        joined(&out).contains("2 applied, 0 pending"),
        "{}",
        joined(&out)
    );
    assert!(joined(&out).contains("no drift"), "{}", joined(&out));
    let out = project.ok(&["--url", &prod, "deploy"]).await;
    assert!(
        joined(&out).contains("nothing to deploy"),
        "{}",
        joined(&out)
    );

    project.schema(V1);
    let (result, out) = project.run(&["sync", "--yes"]).await;
    assert!(
        matches!(result, Err(Error::DestructiveRefused)),
        "dropping a column must need --allow-destructive: {result:?}\n{}",
        joined(&out)
    );
    project.ok(&["sync", "--yes", "--allow-destructive"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generate_base_keeps_one_migration_per_branch() {
    let lease = openmig_testkit::postgres().await;
    let dev = lease.create_database("e2e_branch").await;
    let project = Project::new(&dev);
    project.git(&["init", "-q", "-b", "main"]);
    project.schema(V1);
    project.ok(&["generate", "init"]).await;
    project.git(&["add", "-A"]);
    project.git(&["commit", "-q", "-m", "init"]);
    project.git(&["checkout", "-q", "-b", "feature"]);

    project.schema(V2);
    let out = project.ok(&["generate", "--base", "main", "feature"]).await;
    let after_first = project.migrations();
    assert_eq!(after_first.len(), 2, "{after_first:?}\n{}", joined(&out));

    project.ok(&["generate", "--base", "main", "feature"]).await;
    assert_eq!(
        project.migrations(),
        after_first,
        "regenerating unchanged must keep the file"
    );

    let v3 = V2.replace(
        "view \"app.recent_accounts\"",
        "table \"app.notes\" {\n  column \"body\" {\n    type = text\n  }\n}\n\nview \"app.recent_accounts\"",
    );
    project.schema(&v3);
    project.ok(&["generate", "--base", "main", "feature"]).await;
    let files = project.migrations();
    assert_eq!(
        files.len(),
        2,
        "still one generated migration on the branch: {files:?}"
    );
    let content =
        std::fs::read_to_string(project.path().join("migrations").join(&files[1])).unwrap();
    assert!(content.contains("\"notes\""), "{content}");
    assert!(content.contains("\"created_at\""), "{content}");
    project.ok(&["check", "--base", "main"]).await;
}

/// Answers every prompt with yes and accepts every proposed rename, like a user at a terminal.
#[derive(Default)]
struct YesUi {
    messages: std::sync::Mutex<Vec<String>>,
}

impl openmig::Ui for YesUi {
    fn is_interactive(&self) -> bool {
        true
    }

    fn info(&self, message: &str) {
        self.messages.lock().unwrap().push(message.to_owned());
    }

    fn warn(&self, message: &str) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("warning: {message}"));
    }

    fn show_plan(&self, title: &str, plan: &openmig_core::Plan) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("{title}\n{}", plan.to_sql()));
    }

    fn confirm(&self, _prompt: &str) -> bool {
        true
    }

    fn choose_renames(
        &self,
        candidates: &[openmig_core::RenameCandidate],
    ) -> Vec<openmig_core::RenameCandidate> {
        candidates.to_vec()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_sync_offers_renames() {
    let lease = openmig_testkit::postgres().await;
    let dev = lease.create_database("e2e_rename").await;
    let project = Project::new(&dev);
    project.schema(V1);
    project.ok(&["sync", "--yes"]).await;

    project.schema(&V1.replace("\"email\"", "\"email_address\""));
    let config = project.path().join("openmig.toml");
    let cli =
        Cli::try_parse_from(["openmig", "--config", config.to_str().unwrap(), "sync"]).unwrap();
    let ui = YesUi::default();
    let code = openmig::flows::run(cli, &ui).await.unwrap();
    assert_eq!(code, 0);
    let out = ui.messages.lock().unwrap().join("\n");
    assert!(
        out.contains("RENAME COLUMN \"email\" TO \"email_address\""),
        "{out}"
    );
    assert!(!out.contains("DROP COLUMN"), "{out}");
    assert!(
        out.contains("renamed_from = \"email\""),
        "the hint for CI must be printed:\n{out}"
    );
}
