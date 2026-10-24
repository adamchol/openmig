//! `openmig check`: the migration directory is valid and matches the schema.

use super::common::{Scratch, load_desired, normalize, plan_between, resolve_renames, scope_for};
use super::git;
use crate::cli::CheckArgs;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{Engine, Error, PlanMode, Session};
use openmig_migrate::MigrationDir;

pub async fn check<E: Engine>(
    ctx: &Context<E>,
    args: &CheckArgs,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let dir = MigrationDir::load(ctx.migrations_dir()?)?;
    dir.verify_sum()
        .map_err(|e| Error::Migrations(e.to_string()))?;
    if let Some(base) = &args.base {
        check_against_base(&dir, base)?;
    }
    let desired = load_desired(ctx, ui)?;
    let scope = scope_for(ctx, Some(&desired))?;
    let mut scratch = Scratch::start(ctx, ui).await?;
    let result: Result<u8, Error> = async {
        openmig_migrate::replay::<E>(&mut scratch.session, &dir).await?;
        let current = scratch.session.inspect(&scope).await?;
        scratch.reset().await?;
        let normalized = normalize(&mut scratch.session, ctx, &desired, &scope).await?;
        let renames = resolve_renames::<E>(&current, &normalized, &desired.hints, ui, false);
        let plan = plan_between(
            ctx,
            &scope,
            &current,
            &normalized,
            &renames,
            PlanMode::Generate,
            false,
        )?;
        if !plan.is_empty() {
            ui.show_plan("-- schema changes that no migration contains", &plan);
            return Err(Error::Drift(format!(
                "{}: the schema has changes that no migration contains; run `openmig generate`",
                ctx.name
            )));
        }
        ui.info(&format!(
            "{}: {} migration(s) valid and in sync with the schema",
            ctx.name,
            dir.files.len()
        ));
        Ok(0)
    }
    .await;
    let _ = scratch.stop().await;
    result
}

/// Files that exist in `base` must be unchanged, and new files must sort after all of them.
fn check_against_base(dir: &MigrationDir, base: &str) -> Result<(), Error> {
    let in_base = git::files_in_ref(&dir.path, base)?;
    let mut problems = Vec::new();
    let mut newest_base: Option<&str> = None;
    for file in &dir.files {
        if in_base.contains(&file.file_name) {
            newest_base = Some(&file.version);
            let old = git::file_at_ref(&dir.path, base, &file.file_name)?;
            if old != file.content {
                problems.push(format!(
                    "{} was changed after it was merged",
                    file.file_name
                ));
            }
        }
    }
    for name in &in_base {
        if name.ends_with(".sql") && !dir.files.iter().any(|f| &f.file_name == name) {
            problems.push(format!("{name} exists in {base} but was removed"));
        }
    }
    if let Some(newest) = newest_base {
        for file in &dir.files {
            if !in_base.contains(&file.file_name) && file.version.as_str() <= newest {
                problems.push(format!(
                    "{} is older than {newest}, the newest migration in {base}; regenerate it",
                    file.file_name
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Migrations(format!(
            "migration directory does not extend {base}:\n  {}",
            problems.join("\n  ")
        )))
    }
}
