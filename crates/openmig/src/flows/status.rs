//! `openmig status`.

use super::common::{Scratch, plan_between, scope_for};
use crate::cli::StatusArgs;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{Engine, Error, PlanMode, RenameSet, Session, SessionPurpose};
use openmig_migrate::{MigrationDir, StatusState};

pub async fn status<E: Engine>(
    ctx: &Context<E>,
    args: &StatusArgs,
    ui: &dyn Ui,
    json: bool,
) -> Result<u8, Error> {
    let dir = MigrationDir::load(ctx.migrations_dir()?)?;
    let mut target = E::connect(&ctx.url()?, &ctx.settings, SessionPurpose::Target).await?;
    let history = target.history_load().await?;
    let status = openmig_migrate::status(&history, &dir);
    if json {
        let entries: Vec<_> = status
            .entries
            .iter()
            .map(|e| serde_json::json!({ "version": e.version, "name": e.name, "state": state_name(&e.state) }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "database": ctx.name, "migrations": entries })
        );
    } else {
        for e in &status.entries {
            let detail = match &e.state {
                StatusState::Failed { error: Some(err) } => format!("  ({err})"),
                _ => String::new(),
            };
            println!(
                "{:<14}  {:<10}  {}{detail}",
                e.version,
                state_name(&e.state),
                e.name
            );
        }
    }
    let pending = status.pending().count();
    if !json {
        ui.info(&format!(
            "{}: {} applied, {pending} pending",
            ctx.name,
            status.entries.len() - pending
        ));
    }
    if args.drift {
        let scope = scope_for(ctx, None)?;
        let current = target.inspect(&scope).await?;
        let mut scratch = Scratch::start(ctx, ui).await?;
        let replayed = async {
            openmig_migrate::replay::<E>(&mut scratch.session, &dir).await?;
            scratch.session.inspect(&scope).await
        }
        .await;
        let _ = scratch.stop().await;
        let replayed = replayed?;
        let plan = plan_between(
            ctx,
            &scope,
            &current,
            &replayed,
            &RenameSet::default(),
            PlanMode::Generate,
            false,
        )?;
        if !plan.is_empty() {
            ui.show_plan(
                "-- statements that would make the database match its migrations",
                &plan,
            );
            return Err(Error::Drift(format!(
                "{}: the database differs from its migrations",
                ctx.name
            )));
        }
        ui.info(&format!("{}: no drift", ctx.name));
    }
    if args.fail_on_pending && pending > 0 {
        return Err(Error::Drift(format!(
            "{}: {pending} migration(s) pending",
            ctx.name
        )));
    }
    Ok(0)
}

pub fn state_name(state: &StatusState) -> &'static str {
    match state {
        StatusState::Applied => "applied",
        StatusState::Baseline => "baseline",
        StatusState::Pending => "pending",
        StatusState::Failed { .. } => "failed",
        StatusState::Modified => "modified",
        StatusState::Missing => "missing",
        StatusState::OutOfOrder => "out-of-order",
    }
}
