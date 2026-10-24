//! One module per command. Every flow is generic over `E: Engine`.

pub mod check;
pub mod common;
pub mod deploy;
pub mod diff;
pub mod files;
pub mod generate;
mod git;
pub mod history;
pub mod inspect;
pub mod status;
pub mod sync;

use crate::cli::{Cli, Command};
use crate::context::{Context, EngineKind, load_config, select};
use crate::ui::Ui;
use openmig_config::DatabaseConfig;
use openmig_core::{Engine, Error};
use owo_colors::OwoColorize;

/// Run a parsed command line for every selected database. Returns the exit code.
pub async fn run(cli: Cli, ui: &dyn Ui) -> Result<u8, Error> {
    if cli.global.all && matches!(&cli.command, Command::Sync(a) if a.watch) {
        return Err(Error::Config(
            "`sync --watch` runs for one database; use --db".into(),
        ));
    }
    let config = load_config(&cli.global)?;
    let databases = select(&config, &cli.global)?;
    let mut code = 0;
    for db in databases {
        let engine = EngineKind::parse(&db.engine)
            .ok_or_else(|| Error::Config(format!("unknown engine `{}`", db.engine)))?;
        code = code.max(dispatch(engine, db, &cli, ui).await?);
    }
    Ok(code)
}

async fn dispatch(
    engine: EngineKind,
    db: &DatabaseConfig,
    cli: &Cli,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let _ = (db, cli, ui);
    Err(Error::Config(format!(
        "the {} engine is not available in this build yet",
        engine.as_str()
    )))
}

/// Run one command for one database with engine `E`.
pub async fn run_engine<E: Engine>(
    db: &DatabaseConfig,
    cli: &Cli,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let ctx = Context::<E>::new(db, &cli.global)?;
    let json = cli.global.json;
    match &cli.command {
        Command::Sync(args) => sync::sync(&ctx, args, ui).await,
        Command::Generate(args) => generate::generate(&ctx, args, ui).await,
        Command::Check(args) => check::check(&ctx, args, ui).await,
        Command::Deploy(args) => deploy::deploy(&ctx, args, ui, json).await,
        Command::Status(args) => status::status(&ctx, args, ui, json).await,
        Command::New(args) => files::new(&ctx, args, ui),
        Command::Rehash => files::rehash(&ctx, ui),
        Command::Inspect(args) => inspect::inspect(&ctx, args, ui).await,
        Command::Diff(args) => diff::diff(&ctx, args, ui).await,
        Command::History(cmd) => history::history(&ctx, cmd, ui).await,
    }
}

/// Print an error to stderr.
pub fn report_error(error: &Error, color: bool) {
    let label = if color {
        "error:".red().bold().to_string()
    } else {
        "error:".to_owned()
    };
    match error {
        Error::Schema { rendered } => eprintln!("{}", rendered.trim_end()),
        Error::Unsupported {
            hint: Some(hint), ..
        } => eprintln!("{label} {error}\n  hint: {hint}"),
        _ => eprintln!("{label} {error}"),
    }
}
