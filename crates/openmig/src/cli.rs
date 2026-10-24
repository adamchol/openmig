//! Command-line definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "openmig",
    version,
    about = "Declarative and versioned schema migrations for PostgreSQL and ClickHouse",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args, Clone, Default)]
pub struct GlobalArgs {
    /// Config file (default: search upward for openmig.toml).
    #[arg(long, global = true, env = "OPENMIG_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Database from the config. Optional when the config has only one.
    #[arg(
        short = 'd',
        long = "db",
        global = true,
        env = "OPENMIG_DB",
        value_name = "NAME"
    )]
    pub db: Option<String>,
    /// Run for every database in the config.
    #[arg(long, global = true, conflicts_with = "db")]
    pub all: bool,
    /// Override the database URL.
    #[arg(long, global = true, env = "OPENMIG_URL", value_name = "URL", hide_env_values = true)]
    pub url: Option<String>,
    /// Additional schema file or directory (repeatable).
    #[arg(long = "schema", global = true, value_name = "PATH")]
    pub schema: Vec<PathBuf>,
    /// Schema variable (repeatable).
    #[arg(long = "var", global = true, value_name = "NAME=VALUE", value_parser = parse_var)]
    pub vars: Vec<(String, String)>,
    /// Engine for config-less mode.
    #[arg(long, global = true, value_enum)]
    pub engine: Option<EngineKind>,
    /// Migration directory (config-less mode, or override).
    #[arg(long, global = true, value_name = "DIR")]
    pub migrations: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_enum,
        env = "OPENMIG_LOG_FORMAT",
        default_value = "text"
    )]
    pub log_format: LogFormat,
    /// Machine-readable command output.
    #[arg(long, global = true)]
    pub json: bool,
    /// More logging (repeatable).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Less output.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: ColorChoice,
}

fn parse_var(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_owned(), v.to_owned()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| format!("expected NAME=VALUE, got `{s}`"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineKind {
    Postgres,
    Clickhouse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Make a database match the schema (local development).
    Sync(SyncArgs),
    /// Write a new migration file for the difference between the migrations and the schema.
    Generate(GenerateArgs),
    /// CI gate: the migration directory is valid and matches the schema.
    Check(CheckArgs),
    /// Apply pending migration files.
    Deploy(DeployArgs),
    /// Show applied and pending migrations.
    Status(StatusArgs),
    /// Create an empty migration file for hand-written SQL.
    New(NewArgs),
    /// Rewrite openmig.sum.
    Rehash,
    /// Print a database as HCL (or SQL).
    Inspect(InspectArgs),
    /// Print the SQL that turns one state into another.
    Diff(DiffArgs),
    /// Inspect or repair the migration history table.
    #[command(subcommand)]
    History(HistoryCommand),
}

#[derive(Debug, Args, Clone, Default)]
pub struct SyncArgs {
    /// Re-run whenever schema files or the config change.
    #[arg(long)]
    pub watch: bool,
    /// Apply non-destructive plans without asking.
    #[arg(short, long)]
    pub yes: bool,
    /// Apply destructive statements without asking.
    #[arg(long)]
    pub allow_destructive: bool,
    /// Print the plan without applying it.
    #[arg(long)]
    pub dry_run: bool,
    /// Allow drop + create for changes that cannot be altered in place.
    #[arg(long)]
    pub recreate: bool,
    /// Ignore `data` blocks.
    #[arg(long)]
    pub no_data: bool,
}

#[derive(Debug, Args, Clone, Default)]
pub struct GenerateArgs {
    /// Migration name, e.g. add_users.
    pub name: Option<String>,
    /// Delete generated migration files that are not in this git ref before generating.
    #[arg(long, value_name = "GIT_REF")]
    pub base: Option<String>,
    /// Print the migration without writing it.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone, Default)]
pub struct CheckArgs {
    /// Files in this git ref must be unchanged and new files newer than its newest one.
    #[arg(long, value_name = "GIT_REF")]
    pub base: Option<String>,
}

#[derive(Debug, Args, Clone, Default)]
pub struct DeployArgs {
    /// Stop after this version.
    #[arg(long, value_name = "VERSION")]
    pub to: Option<String>,
    /// On a database without history: record files up to VERSION as applied without running them.
    #[arg(long, value_name = "VERSION")]
    pub baseline: Option<String>,
    /// Allow the first deploy into a database that already has objects.
    #[arg(long)]
    pub allow_dirty: bool,
    /// Apply pending files older than the newest applied one.
    #[arg(long)]
    pub allow_out_of_order: bool,
    /// Print what would run.
    #[arg(long)]
    pub dry_run: bool,
    /// Keep retrying the connection for this long (e.g. 2m).
    #[arg(long, value_name = "DURATION")]
    pub wait_for_db: Option<String>,
    /// How long to wait for the migration lock (default from config, 10m).
    #[arg(long, value_name = "DURATION")]
    pub lock_wait: Option<String>,
}

#[derive(Debug, Args, Clone, Default)]
pub struct StatusArgs {
    /// Also compare the live database with the replayed migrations.
    #[arg(long)]
    pub drift: bool,
    /// Exit with code 4 when migrations are pending.
    #[arg(long)]
    pub fail_on_pending: bool,
}

#[derive(Debug, Args, Clone, Default)]
pub struct NewArgs {
    /// Migration name, e.g. backfill_emails.
    pub name: String,
    /// Mark the file `-- openmig:no-transaction`.
    #[arg(long)]
    pub no_transaction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum InspectFormat {
    #[default]
    Hcl,
    Sql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum SplitByArg {
    #[default]
    None,
    Schema,
    Kind,
}

#[derive(Debug, Args, Clone, Default)]
pub struct InspectArgs {
    #[arg(long, value_enum, default_value = "hcl")]
    pub format: InspectFormat,
    #[arg(long, value_enum, default_value = "none")]
    pub split_by: SplitByArg,
    /// Write files into this directory instead of printing.
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, Default)]
pub struct DiffArgs {
    /// `schema`, `migrations`, `url:<URL>` or a path to HCL files.
    #[arg(long, value_name = "SOURCE")]
    pub from: String,
    /// `schema`, `migrations`, `url:<URL>` or a path to HCL files.
    #[arg(long, value_name = "SOURCE")]
    pub to: String,
}

#[derive(Debug, Subcommand, Clone)]
pub enum HistoryCommand {
    /// List history rows.
    List,
    /// Record a migration as applied without running it.
    MarkApplied { version: String },
    /// Delete a history row.
    Delete { version: String },
}
