//! End-to-end runs of the `openmig` command line against a real ClickHouse server.

use clap::Parser;
use openmig::cli::Cli;
use openmig::term::RecordingUi;
use openmig_core::Error;
use std::path::Path;

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
                "[databases.analytics]\nengine = \"clickhouse\"\nschema = [\"schema\"]\nmigrations = \"migrations\"\nurl = \"{url}\"\n"
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn schema(&self, hcl: &str) {
        std::fs::write(self.path().join("schema/analytics.hcl"), hcl).unwrap();
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

const V1: &str = r#"
database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(3)
  }
  column "kind" {
    type = LowCardinality(String)
  }
  order_by     = [column.timestamp]
  partition_by = "toYYYYMM(timestamp)"
}
"#;

const V2: &str = r#"
database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(3)
  }
  column "kind" {
    type = LowCardinality(String)
  }
  column "payload" {
    type  = String
    codec = [ZSTD(3)]
  }
  order_by     = [column.timestamp]
  partition_by = "toYYYYMM(timestamp)"
  ttl          = "toDateTime(timestamp) + toIntervalDay(30)"
  index "kind_set" {
    expr = "kind"
    type = set(100)
  }
}

table "analytics.kinds" {
  engine = ReplacingMergeTree
  column "kind" {
    type = LowCardinality(String)
  }
  column "last_seen" {
    type = DateTime64(3)
  }
  order_by = [column.kind]
}

materialized_view "analytics.kinds_mv" {
  to = table.analytics.kinds
  as = "SELECT kind, max(timestamp) AS last_seen FROM analytics.events GROUP BY kind"
}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn develop_generate_check_deploy() {
    let lease = openmig_testkit::clickhouse().await;
    let project = Project::new(lease.url());

    project.schema(V1);
    project.ok(&["sync", "--yes"]).await;
    let out = project.ok(&["sync", "--yes"]).await;
    assert!(out.contains("up to date"), "{out}");
    project.ok(&["generate", "init"]).await;
    project.ok(&["check"]).await;

    project.schema(V2);
    let (result, out) = project.run(&["check"]).await;
    assert!(matches!(result, Err(Error::Drift(_))), "{result:?}\n{out}");
    project.ok(&["generate", "events_v2"]).await;
    project.ok(&["check"]).await;
    let out = project.ok(&["sync", "--yes"]).await;
    assert!(out.contains("applied"), "{out}");
    let out = project
        .ok(&["diff", "--from", "migrations", "--to", "schema"])
        .await;
    assert!(out.contains("no differences"), "{out}");

    lease
        .query("DROP DATABASE IF EXISTS analytics SYNC")
        .await
        .unwrap();
    project.ok(&["deploy"]).await;
    let out = project.ok(&["status", "--drift"]).await;
    assert!(out.contains("2 applied, 0 pending"), "{out}");
    assert!(out.contains("no drift"), "{out}");
    let out = project.ok(&["deploy"]).await;
    assert!(out.contains("nothing to deploy"), "{out}");
}
