//! `openmig generate`: write a migration file for the difference between the migrations and the
//! schema.

use super::common::{Scratch, load_desired, normalize, plan_between, resolve_renames, scope_for};
use super::git;
use crate::cli::GenerateArgs;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{Engine, Error, PlanMode, RenameSet, Session};
use openmig_migrate::{MigrationDir, render_file};
use std::path::Path;

pub async fn generate<E: Engine>(
    ctx: &Context<E>,
    args: &GenerateArgs,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let dir_path = ctx.migrations_dir()?.clone();
    if let Some(base) = &args.base {
        remove_unmerged_generated(&dir_path, base, ui)?;
    }
    let mut dir = MigrationDir::load(&dir_path)?;
    dir.verify_sum()
        .map_err(|e| Error::Migrations(e.to_string()))?;
    let desired = load_desired(ctx, ui)?;
    let scope = scope_for(ctx, Some(&desired))?;
    let name = sanitize_name(args.name.as_deref().unwrap_or_default());

    let mut scratch = Scratch::start(ctx, ui).await?;
    let result: Result<u8, Error> = async {
        openmig_migrate::replay::<E>(&mut scratch.session, &dir).await?;
        let current = scratch.session.inspect(&scope).await?;
        scratch.reset().await?;
        let normalized = normalize(&mut scratch.session, ctx, &desired, &scope).await?;
        let renames = resolve_renames::<E>(&current, &normalized, &desired.hints, ui, true);
        let plan = plan_between(
            ctx,
            &scope,
            &current,
            &normalized,
            &renames,
            PlanMode::Generate,
            false,
        )?;
        if plan.is_empty() {
            ui.info(&format!("{}: no changes", ctx.name));
            return Ok(0);
        }
        let content = render_file(&plan, true);
        if args.dry_run {
            print!("{content}");
            return Ok(0);
        }
        let version = dir.next_version(jiff::Timestamp::now());
        let path = dir.add(&version, &name, &content)?.path.clone();

        scratch.reset().await?;
        openmig_migrate::replay::<E>(&mut scratch.session, &dir).await?;
        let after = scratch.session.inspect(&scope).await?;
        let leftover = plan_between(
            ctx,
            &scope,
            &after,
            &normalized,
            &RenameSet::default(),
            PlanMode::Generate,
            false,
        )?;
        if !leftover.is_empty() {
            ui.show_plan("-- not reproduced by the generated migration", &leftover);
            return Err(Error::Drift(format!(
                "replaying {} does not reproduce the schema; this is a bug in OpenMig, please report it",
                path.display()
            )));
        }
        ui.info(&format!(
            "{}: wrote {} ({} statement(s))",
            ctx.name,
            path.display(),
            plan.len()
        ));
        println!("{}", path.display());
        Ok(0)
    }
    .await;
    let _ = scratch.stop().await;
    result
}

/// Delete generated migration files that do not exist in `base`, so a branch carries exactly one
/// generated migration. Hand-written files are kept.
fn remove_unmerged_generated(dir: &Path, base: &str, ui: &dyn Ui) -> Result<(), Error> {
    if !dir.exists() {
        return Ok(());
    }
    let in_base = git::files_in_ref(dir, base)?;
    let current = MigrationDir::load(dir)?;
    let mut removed = 0;
    for file in &current.files {
        if file.directives.generated && !in_base.contains(&file.file_name) {
            std::fs::remove_file(&file.path)
                .map_err(|e| Error::io(format!("removing {}", file.path.display()), e))?;
            ui.info(&format!(
                "removed unmerged generated migration {}",
                file.file_name
            ));
            removed += 1;
        }
    }
    if removed > 0 {
        MigrationDir::load(dir)?.write_sum()?;
    }
    Ok(())
}

/// Lower-case, `[a-z0-9_]` only, no leading/trailing/double underscores.
pub fn sanitize_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sanitize() {
        assert_eq!(super::sanitize_name("Add users-table!"), "add_users_table");
        assert_eq!(super::sanitize_name("  "), "");
        assert_eq!(super::sanitize_name("__x__y"), "x_y");
    }
}
