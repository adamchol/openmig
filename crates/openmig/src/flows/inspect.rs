//! `openmig inspect`: print a database as HCL or SQL.

use super::common::scope_for;
use crate::cli::{InspectArgs, InspectFormat, SplitByArg};
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{
    DiffContext, EncodeOptions, Engine, Error, PlanMode, RenameSet, Session, SessionPurpose,
    SplitBy,
};

pub async fn inspect<E: Engine>(
    ctx: &Context<E>,
    args: &InspectArgs,
    ui: &dyn Ui,
) -> Result<u8, Error> {
    let scope = scope_for(ctx, None)?;
    let mut target = E::connect(&ctx.url()?, &ctx.settings, SessionPurpose::Target).await?;
    let model = target.inspect(&scope).await?;
    let files = match args.format {
        InspectFormat::Hcl => {
            let split = match args.split_by {
                SplitByArg::None => SplitBy::None,
                SplitByArg::Schema => SplitBy::Schema,
                SplitByArg::Kind => SplitBy::Kind,
            };
            E::encode(&model, &ctx.settings, &EncodeOptions { split })
                .into_iter()
                .map(|f| (f.path, f.content))
                .collect::<Vec<_>>()
        }
        InspectFormat::Sql => {
            let empty = E::Model::default();
            let renames = RenameSet::default();
            let dctx = DiffContext::<E> {
                settings: &ctx.settings,
                scope: &scope,
                renames: &renames,
                mode: PlanMode::Generate,
                allow_recreate: false,
            };
            let changes = E::diff(&empty, &model, &dctx)?;
            let plan = E::plan(changes, &empty, &model, &dctx)?;
            vec![("schema.sql".into(), plan.to_sql())]
        }
    };
    match &args.output {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
            for (path, content) in &files {
                let target = dir.join(path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::io(format!("creating {}", parent.display()), e))?;
                }
                std::fs::write(&target, content)
                    .map_err(|e| Error::io(format!("writing {}", target.display()), e))?;
                ui.info(&format!("wrote {}", target.display()));
            }
        }
        None => {
            let many = files.len() > 1;
            for (i, (path, content)) in files.iter().enumerate() {
                if many {
                    if i > 0 {
                        println!();
                    }
                    println!("# file: {}", path.display());
                }
                print!("{content}");
            }
        }
    }
    Ok(0)
}
