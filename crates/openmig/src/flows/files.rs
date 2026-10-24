//! `openmig new` and `openmig rehash`.

use super::generate::sanitize_name;
use crate::cli::NewArgs;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{Engine, Error};
use openmig_migrate::MigrationDir;

pub fn new<E: Engine>(ctx: &Context<E>, args: &NewArgs, ui: &dyn Ui) -> Result<u8, Error> {
    let mut dir = MigrationDir::load(ctx.migrations_dir()?)?;
    dir.verify_sum()
        .map_err(|e| Error::Migrations(e.to_string()))?;
    let name = sanitize_name(&args.name);
    if name.is_empty() {
        return Err(Error::Config(
            "a migration name is required, e.g. `openmig new backfill_emails`".into(),
        ));
    }
    let version = dir.next_version(jiff::Timestamp::now());
    let content = if args.no_transaction {
        "-- openmig:no-transaction\n\n"
    } else {
        ""
    };
    let path = dir.add(&version, &name, content)?.path.clone();
    ui.info(&format!("created {}", path.display()));
    println!("{}", path.display());
    Ok(0)
}

pub fn rehash<E: Engine>(ctx: &Context<E>, ui: &dyn Ui) -> Result<u8, Error> {
    let dir = MigrationDir::load(ctx.migrations_dir()?)?;
    dir.write_sum()?;
    ui.info(&format!(
        "{}: wrote {} for {} migration(s)",
        ctx.name,
        openmig_migrate::SUM_FILE_NAME,
        dir.files.len()
    ));
    Ok(0)
}
