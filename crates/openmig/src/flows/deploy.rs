//! `openmig deploy`: apply pending migration files.

use crate::cli::DeployArgs;
use crate::context::Context;
use crate::ui::Ui;
use crate::util::parse_duration;
use openmig_core::{Engine, Error, SessionPurpose};
use openmig_migrate::{DeployOptions, FileOutcome, MigrationDir, MigrationFile, Progress};
use std::time::{Duration, Instant};

pub async fn deploy<E: Engine>(
    ctx: &Context<E>,
    args: &DeployArgs,
    ui: &dyn Ui,
    json: bool,
) -> Result<u8, Error> {
    let dir = MigrationDir::load(ctx.migrations_dir()?)?;
    let url = ctx.url()?;
    let wait = args
        .wait_for_db
        .as_deref()
        .map(parse_duration)
        .transpose()
        .map_err(Error::Config)?;
    let mut session = connect_with_retry::<E>(&url, ctx, wait, ui).await?;
    let d = &ctx.config.deploy;
    #[allow(clippy::needless_update)]
    let options = DeployOptions {
        to: args.to.clone(),
        baseline: args.baseline.clone(),
        allow_dirty: args.allow_dirty,
        allow_out_of_order: args.allow_out_of_order || d.allow_out_of_order,
        dry_run: args.dry_run,
        lock_timeout: parse_duration(&d.lock_timeout).map_err(Error::Config)?,
        retries: d.retries,
        lock_wait: parse_duration(args.lock_wait.as_deref().unwrap_or(&d.lock_wait))
            .map_err(Error::Config)?,
        applied_by: applied_by(),
        record_history: true,
        ..DeployOptions::default()
    };
    let mut progress = UiProgress { ui };
    let report = openmig_migrate::deploy::<E>(&mut session, &dir, &options, &mut progress).await?;
    if json {
        let files: Vec<_> = report
            .files
            .iter()
            .map(|(name, outcome)| serde_json::json!({ "file": name, "outcome": outcome_name(outcome) }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "database": ctx.name, "files": files })
        );
    } else if report.files.is_empty() {
        ui.info(&format!("{}: nothing to deploy", ctx.name));
    } else {
        ui.info(&format!(
            "{}: {} migration(s) {}",
            ctx.name,
            report.files.len(),
            if args.dry_run { "pending" } else { "deployed" }
        ));
    }
    Ok(0)
}

fn outcome_name(outcome: &FileOutcome) -> &'static str {
    match outcome {
        FileOutcome::Applied { .. } => "applied",
        FileOutcome::Resumed { .. } => "resumed",
        FileOutcome::Baselined => "baselined",
        FileOutcome::WouldApply { .. } => "pending",
    }
}

/// Connect, retrying connection errors until `wait` expires (for Kubernetes jobs that start
/// before the database).
pub async fn connect_with_retry<E: Engine>(
    url: &str,
    ctx: &Context<E>,
    wait: Option<Duration>,
    ui: &dyn Ui,
) -> Result<E::Session, Error> {
    let deadline = wait.map(|w| Instant::now() + w);
    let mut delay = Duration::from_millis(500);
    loop {
        match E::connect(url, &ctx.settings, SessionPurpose::Deploy).await {
            Ok(s) => return Ok(s),
            Err(Error::Connect { target, message }) => match deadline {
                Some(d) if Instant::now() + delay < d => {
                    ui.info(&format!("waiting for {target}: {message}"));
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
                _ => return Err(Error::Connect { target, message }),
            },
            Err(e) => return Err(e),
        }
    }
}

/// `openmig/<version> host=<host> user=<user>` for the history table.
pub fn applied_by() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".into());
    format!(
        "openmig/{} host={host} user={user}",
        env!("CARGO_PKG_VERSION")
    )
}

struct UiProgress<'a> {
    ui: &'a dyn Ui,
}

impl Progress for UiProgress<'_> {
    fn file_started(&mut self, file: &MigrationFile, statements: usize) {
        tracing::info!(file = %file.file_name, statements, "applying migration");
    }

    fn statement_done(&mut self, file: &MigrationFile, index: usize) {
        tracing::debug!(file = %file.file_name, statement = index, "statement applied");
    }

    fn file_done(&mut self, file: &MigrationFile, outcome: &FileOutcome) {
        let text = match outcome {
            FileOutcome::Applied {
                statements,
                retries: 0,
            } => {
                format!("applied {} ({statements} statements)", file.file_name)
            }
            FileOutcome::Applied {
                statements,
                retries,
            } => format!(
                "applied {} ({statements} statements, {retries} lock retries)",
                file.file_name
            ),
            FileOutcome::Resumed {
                skipped,
                statements,
            } => format!(
                "resumed {} after {skipped} applied statements ({statements} total)",
                file.file_name
            ),
            FileOutcome::Baselined => format!("baselined {}", file.file_name),
            FileOutcome::WouldApply { statements } => {
                format!("pending {} ({statements} statements)", file.file_name)
            }
        };
        self.ui.info(&text);
    }

    fn retrying(&mut self, file: &MigrationFile, attempt: u32, error: &str) {
        self.ui.warn(&format!(
            "{}: attempt {attempt} failed ({error}); retrying",
            file.file_name
        ));
    }
}
