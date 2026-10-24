//! `openmig diff --from SRC --to SRC`.

use super::common::{Scratch, load_desired, normalize, plan_between, scope_for};
use crate::cli::DiffArgs;
use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{Desired, Engine, Error, PlanMode, RenameSet, Scope, Session, SessionPurpose};
use openmig_migrate::MigrationDir;
use std::path::PathBuf;

enum Source {
    Schema,
    Migrations,
    Url(String),
    Path(PathBuf),
}

impl Source {
    fn parse(s: &str) -> Self {
        match s {
            "schema" => Source::Schema,
            "migrations" => Source::Migrations,
            _ => match s.strip_prefix("url:") {
                Some(url) => Source::Url(url.to_owned()),
                None => Source::Path(PathBuf::from(s)),
            },
        }
    }

    fn needs_scratch(&self) -> bool {
        !matches!(self, Source::Url(_))
    }
}

pub async fn diff<E: Engine>(ctx: &Context<E>, args: &DiffArgs, ui: &dyn Ui) -> Result<u8, Error> {
    let from = Source::parse(&args.from);
    let to = Source::parse(&args.to);
    let from_desired = desired_for(ctx, &from, ui)?;
    let to_desired = desired_for(ctx, &to, ui)?;
    let mut scope = scope_for(ctx, None)?;
    for d in [&from_desired, &to_desired].into_iter().flatten() {
        let s = scope_for(ctx, Some(d))?;
        for r in s.declared_roles {
            if !scope.declared_roles.contains(&r) {
                scope.declared_roles.push(r);
            }
        }
        for t in s.data_tables {
            if !scope.data_tables.contains(&t) {
                scope.data_tables.push(t);
            }
        }
    }
    let mut scratch = if from.needs_scratch() || to.needs_scratch() {
        Some(Scratch::start(ctx, ui).await?)
    } else {
        None
    };
    let result: Result<u8, Error> = async {
        let from_model =
            model_for(ctx, &from, from_desired.as_ref(), &scope, scratch.as_mut()).await?;
        let to_model = model_for(ctx, &to, to_desired.as_ref(), &scope, scratch.as_mut()).await?;
        let renames = match &to_desired {
            Some(d) => E::resolve_hints(&from_model, &to_model, &d.hints),
            None => RenameSet::default(),
        };
        let plan = plan_between(
            ctx,
            &scope,
            &from_model,
            &to_model,
            &renames,
            PlanMode::Generate,
            false,
        )?;
        if plan.is_empty() {
            ui.info("no differences");
        } else {
            print!("{}", plan.to_sql());
        }
        Ok(0)
    }
    .await;
    if let Some(s) = scratch {
        let _ = s.stop().await;
    }
    result
}

fn desired_for<E: Engine>(
    ctx: &Context<E>,
    source: &Source,
    ui: &dyn Ui,
) -> Result<Option<Desired<E::Model>>, Error> {
    match source {
        Source::Schema => load_desired(ctx, ui).map(Some),
        Source::Path(p) => {
            let mut c = ctx.clone();
            c.schema_paths = vec![p.clone()];
            load_desired(&c, ui).map(Some)
        }
        _ => Ok(None),
    }
}

async fn model_for<E: Engine>(
    ctx: &Context<E>,
    source: &Source,
    desired: Option<&Desired<E::Model>>,
    scope: &Scope,
    scratch: Option<&mut Scratch<E>>,
) -> Result<E::Model, Error> {
    match source {
        Source::Url(url) => {
            let mut s = E::connect(url, &ctx.settings, SessionPurpose::Target).await?;
            s.inspect(scope).await
        }
        Source::Migrations => {
            let scratch = scratch.expect("scratch started for migrations");
            scratch.reset().await?;
            let dir = MigrationDir::load(ctx.migrations_dir()?)?;
            openmig_migrate::replay::<E>(&mut scratch.session, &dir).await?;
            scratch.session.inspect(scope).await
        }
        Source::Schema | Source::Path(_) => {
            let scratch = scratch.expect("scratch started for schema");
            scratch.reset().await?;
            normalize(
                &mut scratch.session,
                ctx,
                desired.expect("desired loaded"),
                scope,
            )
            .await
        }
    }
}
