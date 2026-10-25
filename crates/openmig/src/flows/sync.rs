//! `openmig sync`: make a database match the schema.

use super::common::{
    Scratch, apply_plan, check_versions, load_desired, normalize, plan_between, resolve_renames,
    scope_for,
};
use crate::cli::SyncArgs;
use crate::context::Context;
use crate::ui::Ui;
use notify_debouncer_full::notify::RecursiveMode;
use openmig_core::{Engine, Error, PlanMode, RenameSet, Session, SessionPurpose};
use std::time::Duration;

pub async fn sync<E: Engine>(ctx: &Context<E>, args: &SyncArgs, ui: &dyn Ui) -> Result<u8, Error> {
    if args.watch {
        return watch(ctx, args, ui).await;
    }
    let mut scratch = Scratch::start(ctx, ui).await?;
    let result = sync_once(ctx, args, ui, &mut scratch).await;
    let _ = scratch.stop().await;
    result
}

async fn sync_once<E: Engine>(
    ctx: &Context<E>,
    args: &SyncArgs,
    ui: &dyn Ui,
    scratch: &mut Scratch<E>,
) -> Result<u8, Error> {
    let desired = load_desired(ctx, ui)?;
    let mut scope = scope_for(ctx, Some(&desired))?;
    if args.no_data {
        scope.data_tables.clear();
    }
    let url = ctx.url()?;
    let mut target = E::connect(&url, &ctx.settings, SessionPurpose::Target).await?;
    check_versions(
        ui,
        scratch.version(),
        target.server_version().major,
        E::NAME,
    );

    scratch.reset().await?;
    let normalized = normalize(&mut scratch.session, ctx, &desired, &scope).await?;
    let current = target.inspect(&scope).await?;
    let renames = resolve_renames::<E>(&current, &normalized, &desired.hints, ui, true);
    let plan = plan_between(
        ctx,
        &scope,
        &current,
        &normalized,
        &renames,
        PlanMode::Sync,
        args.recreate,
    )?;
    if plan.is_empty() {
        ui.info(&format!("{}: up to date", ctx.name));
        return Ok(0);
    }
    ui.show_plan(
        &format!("-- {}: {} statement(s)", ctx.name, plan.len()),
        &plan,
    );
    if args.dry_run {
        return Ok(0);
    }
    let destructive = plan.destructive().count();
    if destructive > 0 && !args.allow_destructive {
        if !ui.confirm(&format!(
            "Apply {} statement(s), {destructive} of them destructive?",
            plan.len()
        )) {
            return Err(Error::DestructiveRefused);
        }
    } else if !args.yes {
        if !ui.is_interactive() {
            return Err(Error::Config(
                "not running in a terminal; pass --yes to apply changes without confirmation"
                    .into(),
            ));
        }
        if !ui.confirm(&format!("Apply {} statement(s)?", plan.len())) {
            ui.info("not applied");
            return Ok(0);
        }
    }
    apply_plan::<E>(&mut target, &plan).await?;

    let after = target.inspect(&scope).await?;
    let leftover = plan_between(
        ctx,
        &scope,
        &after,
        &normalized,
        &RenameSet::default(),
        PlanMode::Sync,
        args.recreate,
    )?;
    if !leftover.is_empty() {
        ui.warn(
            "the database still differs from the schema after applying the plan; this is a bug in OpenMig, please report it with the statements below",
        );
        ui.show_plan("-- still pending", &leftover);
    }
    ui.info(&format!(
        "{}: applied {} statement(s)",
        ctx.name,
        plan.len()
    ));
    Ok(0)
}

async fn watch<E: Engine>(ctx: &Context<E>, args: &SyncArgs, ui: &dyn Ui) -> Result<u8, Error> {
    let mut scratch = Scratch::start(ctx, ui).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut debouncer = notify_debouncer_full::new_debouncer(
        Duration::from_millis(300),
        None,
        move |result: notify_debouncer_full::DebounceEventResult| {
            if let Ok(events) = result
                && events.iter().any(|e| !e.kind.is_access())
            {
                let _ = tx.send(());
            }
        },
    )
    .map_err(|e| Error::other(format!("cannot watch schema files: {e}")))?;
    for path in ctx.require_schema()? {
        debouncer
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| Error::other(format!("cannot watch {}: {e}", path.display())))?;
    }
    loop {
        if let Err(e) = sync_once(ctx, args, ui, &mut scratch).await {
            super::report_error(&e, false);
        }
        ui.info("watching schema files for changes (Ctrl-C to stop)");
        if rx.recv().await.is_none() {
            break;
        }
        while rx.try_recv().is_ok() {}
    }
    drop(debouncer);
    let _ = scratch.stop().await;
    Ok(0)
}
