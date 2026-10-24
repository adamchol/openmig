//! `openmig history`: inspect and repair the history table.

use super::deploy::applied_by;
use crate::cli::HistoryCommand;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{
    Engine, Error, HistoryKind, HistoryRow, HistoryStatus, Session, SessionPurpose,
};
use openmig_migrate::MigrationDir;

pub async fn history<E: Engine>(
    ctx: &Context<E>,
    cmd: &HistoryCommand,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let mut session = E::connect(&ctx.url()?, &ctx.settings, SessionPurpose::Deploy).await?;
    match cmd {
        HistoryCommand::List => {
            for row in session.history_load().await? {
                println!(
                    "{:<14}  {:<9}  {:<9}  {}/{}  {}{}",
                    row.version,
                    row.kind.as_str(),
                    row.status.as_str(),
                    row.statements_applied,
                    row.statements_total,
                    row.name,
                    row.error.map(|e| format!("  ({e})")).unwrap_or_default()
                );
            }
        }
        HistoryCommand::MarkApplied { version } => {
            let dir = MigrationDir::load(ctx.migrations_dir()?)?;
            let file = dir
                .files
                .iter()
                .find(|f| &f.version == version)
                .ok_or_else(|| {
                    Error::Migrations(format!("no migration file with version {version}"))
                })?;
            let statements = openmig_core::sql::split(&file.content, E::DIALECT)
                .map_err(|e| Error::Migrations(format!("{}: {e}", file.file_name)))?;
            let checksums: Vec<String> = statements
                .iter()
                .map(|s| openmig_migrate::sha256_hex(s.sql.as_bytes()))
                .collect();
            let now = jiff::Timestamp::now();
            session.history_ensure().await?;
            session
                .history_write(&HistoryRow {
                    version: file.version.clone(),
                    name: file.name.clone(),
                    checksum: file.checksum.clone(),
                    kind: HistoryKind::Migration,
                    transactional: !file.directives.no_transaction,
                    statements_total: checksums.len() as u32,
                    statements_applied: checksums.len() as u32,
                    statement_checksums: checksums,
                    status: HistoryStatus::Applied,
                    error: None,
                    error_statement: None,
                    started_at: now,
                    finished_at: Some(now),
                    applied_by: format!("{} (mark-applied)", applied_by()),
                })
                .await?;
            ui.info(&format!("marked {} as applied", file.file_name));
        }
        HistoryCommand::Delete { version } => {
            session.history_delete(version).await?;
            ui.info(&format!("deleted history row {version}"));
        }
    }
    Ok(0)
}
