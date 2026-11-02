//! Helpers shared by the flows: loading the desired schema, scratch servers, normalization and
//! applying plans.

use crate::context::Context;
use crate::ui::Ui;
use openmig_core::{
    Desired, DiffContext, Engine, Error, ExecError, ExecErrorKind, ObjectKind, ObjectRef, Owned,
    Plan, PlanMode, RenameSet, SchemaFilter, Scope, ScratchPrelude, Session, SessionPurpose,
    Statement,
};
use openmig_hcl::Span;
use openmig_hcl::{Diagnostic, Diagnostics, LoadOptions};
use openmig_scratch::ScratchServer;

/// Load and decode the schema files of a database.
pub fn load_desired<E: Engine>(ctx: &Context<E>, ui: &dyn Ui) -> Result<Desired<E::Model>, Error> {
    let paths = ctx.require_schema()?;
    let spec = E::schema_spec(&ctx.settings);
    let doc = openmig_hcl::load(
        paths,
        &LoadOptions {
            spec: &spec,
            vars: ctx.vars.clone(),
        },
    )
    .map_err(|e| Error::Schema {
        rendered: e.render(false),
    })?;
    if !doc.warnings.is_empty() {
        ui.warn(doc.warnings.render(&doc.sources, false).trim_end());
    }
    E::decode(&doc, &ctx.settings)
}

/// The scope for a database with the desired schema's roles and data tables filled in.
pub fn scope_for<E: Engine>(
    ctx: &Context<E>,
    desired: Option<&Desired<E::Model>>,
) -> Result<Scope, Error> {
    let mut scope = ctx.scope()?;
    if let Some(d) = desired {
        check_declared_in_scope(d, &scope)?;
        scope.owned = Owned::only(E::owned(&d.model));
        let default = match ctx.config.data {
            openmig_config::DataMode::Off => None,
            openmig_config::DataMode::Insert => Some(openmig_core::DataSync::Insert),
            openmig_config::DataMode::Sync => Some(openmig_core::DataSync::Sync),
        };
        if let Some(default) = default {
            scope.data_tables = E::data_tables(&d.model)
                .into_iter()
                .map(|mut t| {
                    t.mode.get_or_insert(default);
                    t
                })
                .collect();
        }
    }
    Ok(scope)
}

/// Fail when the schema declares a schema or database (or an object in one) that `schemas` leaves
/// out: it would silently never be created. The implicit `public`/`default` and default namespace
/// have no declaration and are restricted away instead.
fn check_declared_in_scope<M>(desired: &Desired<M>, scope: &Scope) -> Result<(), Error> {
    let SchemaFilter::Only(list) = &scope.schemas else {
        return Ok(());
    };
    let mut outside: Vec<(&ObjectRef, &Span)> = desired
        .origins
        .iter()
        .filter(|(o, _)| matches!(o.kind, ObjectKind::Schema | ObjectKind::Database))
        .filter(|(o, _)| !scope.schemas.includes(&o.path[0]))
        .collect();
    if outside.is_empty() {
        return Ok(());
    }
    outside.sort_by_key(|(_, span)| (span.file, span.start));
    let list = list
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let diags = Diagnostics {
        items: outside
            .into_iter()
            .map(|(o, span)| {
                Diagnostic::error(format!(
                    "{} `{}` is outside `schemas = [{list}]`",
                    o.kind.as_str(),
                    o.path[0]
                ))
                .at(*span)
                .with_label("declared here")
                .with_help("add it to `schemas` in openmig.toml, or qualify the object with a listed schema")
            })
            .collect(),
    };
    Err(Error::Schema {
        rendered: diags.render(&desired.sources, false),
    })
}

/// A scratch server and a session connected to its working database.
pub struct Scratch<E: Engine> {
    server: ScratchServer,
    pub session: E::Session,
    prelude: ScratchPrelude,
}

impl<E: Engine> Scratch<E> {
    pub async fn start(ctx: &Context<E>, ui: &dyn Ui) -> Result<Self, Error> {
        let spec = E::scratch_spec(&ctx.settings, ctx.config.version);
        tracing::info!("starting scratch {} server", E::NAME);
        let server = ScratchServer::start(&spec).await?;
        ui.info(&format!("scratch {} {} started", E::NAME, server.version()));
        let mut session = E::connect(server.url(), &ctx.settings, SessionPurpose::Scratch).await?;
        session.reset().await?;
        Ok(Self {
            server,
            session,
            prelude: ScratchPrelude::default(),
        })
    }

    /// A scratch server prepared for replaying migrations of `desired` (see
    /// [`Engine::scratch_prelude`]); the prelude is applied again after every reset.
    pub async fn start_for(
        ctx: &Context<E>,
        ui: &dyn Ui,
        desired: Option<&Desired<E::Model>>,
    ) -> Result<Self, Error> {
        let mut scratch = Self::start(ctx, ui).await?;
        if let Some(d) = desired {
            scratch.prelude = E::scratch_prelude(&d.model);
            scratch.apply_prelude().await?;
        }
        Ok(scratch)
    }

    /// The scope for inspecting this server: everything on it is OpenMig's but the prelude's
    /// placeholders.
    pub fn scope(&self, scope: &Scope) -> Scope {
        scope.for_scratch(&self.prelude)
    }

    pub fn version(&self) -> &str {
        self.server.version()
    }

    pub async fn reset(&mut self) -> Result<(), Error> {
        self.session.reset().await?;
        self.apply_prelude().await
    }

    async fn apply_prelude(&mut self) -> Result<(), Error> {
        for sql in &self.prelude.statements {
            self.session.execute(sql).await.map_err(|mut e| {
                e.statement.get_or_insert_with(|| sql.clone());
                Error::Exec(e)
            })?;
        }
        Ok(())
    }

    pub async fn stop(self) -> Result<(), Error> {
        drop(self.session);
        self.server.stop().await.map_err(Error::from)
    }
}

/// Apply the desired schema to a pristine scratch database and inspect it back: the desired state
/// in the database's canonical form. The plan starts from the inspected pristine database, which
/// is not empty (PostgreSQL has a `public` schema).
pub async fn normalize<E: Engine>(
    session: &mut E::Session,
    ctx: &Context<E>,
    desired: &Desired<E::Model>,
    scope: &Scope,
) -> Result<E::Model, Error> {
    let pristine = session.inspect(scope).await?;
    // Implicit namespaces outside the scope (`public`) exist in every pristine database but are
    // invisible to inspection, so the diff would try to create them.
    let mut model = desired.model.clone();
    E::restrict(&mut model, scope);
    let renames = RenameSet::default();
    let dctx = DiffContext::<E> {
        settings: &ctx.settings,
        scope,
        renames: &renames,
        mode: PlanMode::Scratch,
        allow_recreate: false,
    };
    let changes = E::diff(&pristine, &model, &dctx)?;
    let plan = E::plan(changes, &pristine, &model, &dctx)?;
    apply_with_retry::<E>(session, &plan, desired).await?;
    let mut normalized = session.inspect(scope).await?;
    E::complete_normalized(&mut normalized, &desired.model);
    Ok(normalized)
}

/// Apply a plan built from decoded HCL. Statements that fail because an object they reference
/// does not exist yet are retried after the others; errors point at the HCL declaration.
pub async fn apply_with_retry<E: Engine>(
    session: &mut E::Session,
    plan: &Plan,
    desired: &Desired<E::Model>,
) -> Result<(), Error> {
    let caps = session.capabilities();
    let transactional =
        caps.transactional_ddl && caps.savepoints && !plan.requires_no_transaction();
    let fail = |stmt: &Statement, err: &ExecError| schema_error(desired, stmt, err);
    if transactional {
        session.begin().await.map_err(Error::Exec)?;
    }
    let mut queue: Vec<&Statement> = plan.statements.iter().collect();
    loop {
        let mut deferred: Vec<(&Statement, ExecError)> = Vec::new();
        let before = queue.len();
        for stmt in queue {
            let result = if transactional {
                run_in_savepoint::<E>(session, &stmt.sql).await
            } else {
                session.execute(&stmt.sql).await
            };
            match result {
                Ok(()) => {}
                Err(e) if e.kind == ExecErrorKind::UndefinedObject => deferred.push((stmt, e)),
                Err(e) => {
                    if transactional {
                        let _ = session.rollback().await;
                    }
                    return Err(fail(stmt, &e));
                }
            }
        }
        if deferred.is_empty() {
            break;
        }
        if deferred.len() == before {
            if transactional {
                let _ = session.rollback().await;
            }
            return Err(schema_errors(desired, &deferred));
        }
        queue = deferred.into_iter().map(|(s, _)| s).collect();
    }
    if transactional {
        session.commit().await.map_err(Error::Exec)?;
    }
    Ok(())
}

async fn run_in_savepoint<E: Engine>(session: &mut E::Session, sql: &str) -> Result<(), ExecError> {
    session.execute("SAVEPOINT openmig_statement").await?;
    match session.execute(sql).await {
        Ok(()) => session.execute("RELEASE SAVEPOINT openmig_statement").await,
        Err(e) => {
            session
                .execute("ROLLBACK TO SAVEPOINT openmig_statement")
                .await?;
            Err(e)
        }
    }
}

fn diagnostic<M>(desired: &Desired<M>, stmt: &Statement, err: &ExecError) -> Diagnostic {
    let mut d = Diagnostic::error(format!("the database rejected the schema: {}", err.message));
    if let Some(span) = stmt.origin.as_ref().and_then(|o| desired.origins.get(o)) {
        d = d.at(*span).with_label("declared here");
    }
    let mut help = String::new();
    if let Some(detail) = &err.detail {
        help.push_str(&format!("detail: {detail}\n"));
    }
    if let Some(hint) = &err.hint {
        help.push_str(&format!("hint: {hint}\n"));
    }
    help.push_str(&format!("statement:\n{}", stmt.sql));
    d.with_help(help)
}

fn schema_error<M>(desired: &Desired<M>, stmt: &Statement, err: &ExecError) -> Error {
    let diags = Diagnostics {
        items: vec![diagnostic(desired, stmt, err)],
    };
    Error::Schema {
        rendered: diags.render(&desired.sources, false),
    }
}

fn schema_errors<M>(desired: &Desired<M>, failures: &[(&Statement, ExecError)]) -> Error {
    let diags = Diagnostics {
        items: failures
            .iter()
            .map(|(s, e)| diagnostic(desired, s, e))
            .collect(),
    };
    Error::Schema {
        rendered: diags.render(&desired.sources, false),
    }
}

/// Apply a plan to a database: in one transaction when the engine supports transactional DDL
/// and no statement forbids it, otherwise statement by statement.
pub async fn apply_plan<E: Engine>(session: &mut E::Session, plan: &Plan) -> Result<(), Error> {
    let caps = session.capabilities();
    let transactional = caps.transactional_ddl && !plan.requires_no_transaction();
    if transactional {
        session.begin().await.map_err(Error::Exec)?;
    }
    for stmt in &plan.statements {
        if let Err(e) = session.execute(&stmt.sql).await {
            if transactional {
                let _ = session.rollback().await;
            }
            let mut e = e;
            e.statement.get_or_insert_with(|| stmt.sql.clone());
            return Err(Error::Exec(e));
        }
    }
    if transactional {
        session.commit().await.map_err(Error::Exec)?;
    }
    Ok(())
}

/// Warn when the scratch server and the target run different major versions: canonical forms
/// can differ between majors and produce spurious changes.
pub fn check_versions(ui: &dyn Ui, scratch_version: &str, target_major: u32, engine: &str) {
    let scratch_major = scratch_version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok());
    if let Some(m) = scratch_major
        && m != target_major
    {
        ui.warn(&format!(
            "the scratch {engine} server is version {scratch_version} but the target runs major version {target_major}; set `version` and point `scratch` at matching binaries to avoid spurious differences"
        ));
    }
}

/// Renames from `renamed_from` hints, plus interactively confirmed candidates.
pub fn resolve_renames<E: Engine>(
    current: &E::Model,
    desired: &E::Model,
    hints: &openmig_core::RenameHints,
    ui: &dyn Ui,
    interactive: bool,
) -> RenameSet {
    let mut renames = E::resolve_hints(current, desired, hints);
    if interactive && ui.is_interactive() {
        let candidates: Vec<_> = E::rename_candidates(current, desired)
            .into_iter()
            .filter(|c| {
                !renames
                    .0
                    .iter()
                    .any(|(from, to)| from == &c.from || to == &c.to)
            })
            .collect();
        for c in ui.choose_renames(&candidates) {
            ui.info(&format!(
                "add `renamed_from = \"{}\"` to {} so CI applies the same rename",
                c.from.name(),
                c.to
            ));
            renames.push(c.from, c.to);
        }
    }
    renames
}

/// Diff two models and plan the result.
pub fn plan_between<E: Engine>(
    ctx: &Context<E>,
    scope: &Scope,
    current: &E::Model,
    desired: &E::Model,
    renames: &RenameSet,
    mode: PlanMode,
    allow_recreate: bool,
) -> Result<Plan, Error> {
    let dctx = DiffContext::<E> {
        settings: &ctx.settings,
        scope,
        renames,
        mode,
        allow_recreate,
    };
    let changes = E::diff(current, desired, &dctx)?;
    E::plan(changes, current, desired, &dctx)
}
