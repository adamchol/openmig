//! Golden convergence cases, generic over the engine.
//!
//! A case is a directory `tests/cases/<engine>/<group>/<name>/` with:
//! * `before.hcl` (optional; empty database when absent) and `after.hcl`,
//! * `expected.sql`: the plan from `before` to `after` (written when missing or with
//!   `OPENMIG_BLESS=1`),
//! * `case.toml` (optional): `[settings]` engine settings, `manage_roles`, `reverse = false`,
//!   `roundtrip = false`, `error = "unsupported"` (the forward plan must fail with that error kind).
//!
//! Each case checks, against real servers:
//! 1. the target converges to `before` (applying `empty -> before`, then re-planning is empty);
//! 2. the plan `before -> after` matches `expected.sql`;
//! 3. after applying it the target converges to `after`;
//! 4. the reverse plan `after -> before` converges too;
//! 5. `after` survives a round trip through `inspect` output: encode, load, normalize, re-plan empty.

#![allow(dead_code)]

use openmig::context::Context;
use openmig::flows::common::{apply_plan, load_desired, normalize, plan_between, scope_for};
use openmig::term::RecordingUi;
use openmig_config::DatabaseConfig;
use openmig_core::{EncodeOptions, Engine, Error, PlanMode, RenameSet, Scope, Session, SessionPurpose};
use std::any::Any;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Postgres,
    ClickHouse,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Postgres => "postgres",
            Kind::ClickHouse => "clickhouse",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    pub dir: PathBuf,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
struct CaseOptions {
    settings: toml::Table,
    manage_roles: bool,
    reverse: Option<bool>,
    roundtrip: Option<bool>,
    error: Option<String>,
}

pub fn discover(kind: Kind) -> Vec<Case> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cases")
        .join(kind.dir());
    let mut cases = Vec::new();
    collect(&root, &root, &mut cases);
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<Case>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.join("after.hcl").is_file() {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(Case { name, dir: path });
            } else {
                collect(root, &path, out);
            }
        }
    }
}

static PAIR: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Two exclusive servers: one used as the scratch server, one as the target.
async fn lease_pair(kind: Kind) -> (Vec<Box<dyn Any + Send>>, String, String) {
    let _guard = PAIR.lock().await;
    let mut leases: Vec<Box<dyn Any + Send>> = Vec::new();
    let mut urls = Vec::new();
    for _ in 0..2 {
        match kind {
            Kind::Postgres => {
                let lease = openmig_testkit::postgres().await;
                urls.push(lease.url().to_owned());
                leases.push(Box::new(lease));
            }
            Kind::ClickHouse => {
                let lease = openmig_testkit::clickhouse().await;
                urls.push(lease.url().to_owned());
                leases.push(Box::new(lease));
            }
        }
    }
    let target = urls.pop().unwrap();
    let scratch = urls.pop().unwrap();
    (leases, scratch, target)
}

fn bless() -> bool {
    std::env::var_os("OPENMIG_BLESS").is_some_and(|v| v != "0")
}

pub async fn run_case<E: Engine>(kind: Kind, case: &Case) -> Result<(), String> {
    let options: CaseOptions = match std::fs::read_to_string(case.dir.join("case.toml")) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("case.toml: {e}"))?,
        Err(_) => CaseOptions::default(),
    };
    let mut db = DatabaseConfig::bare("case", E::NAME);
    db.engine_settings = options.settings.clone();
    db.manage_roles = options.manage_roles;
    let base = Context::<E>::new(&db, &Default::default()).map_err(|e| e.to_string())?;
    let ui = RecordingUi::default();

    let (_leases, scratch_url, target_url) = lease_pair(kind).await;
    let mut scratch = E::connect(&scratch_url, &base.settings, SessionPurpose::Scratch)
        .await
        .map_err(|e| format!("connect scratch: {e}"))?;
    let mut target = E::connect(&target_url, &base.settings, SessionPurpose::Scratch)
        .await
        .map_err(|e| format!("connect target: {e}"))?;
    scratch.reset().await.map_err(|e| format!("reset scratch: {e}"))?;
    target.reset().await.map_err(|e| format!("reset target: {e}"))?;

    let before_path = case.dir.join("before.hcl");
    let after_path = case.dir.join("after.hcl");
    let load = |path: &Path| -> Result<_, String> {
        let mut ctx = base.clone();
        ctx.schema_paths = vec![path.to_path_buf()];
        let desired = load_desired(&ctx, &ui).map_err(|e| render(&e))?;
        Ok((ctx, desired))
    };
    let (after_ctx, after_desired) = load(&after_path)?;
    let before = if before_path.is_file() {
        Some(load(&before_path)?)
    } else {
        None
    };
    let mut scope = scope_for(&after_ctx, Some(&after_desired)).map_err(|e| e.to_string())?;
    if let Some((ctx, d)) = &before {
        let s = scope_for(ctx, Some(d)).map_err(|e| e.to_string())?;
        merge_scope(&mut scope, s);
    }

    let before_model = match &before {
        Some((ctx, d)) => {
            scratch.reset().await.map_err(|e| e.to_string())?;
            normalize(&mut scratch, ctx, d, &scope)
                .await
                .map_err(|e| format!("normalize before.hcl: {}", render(&e)))?
        }
        None => {
            scratch.reset().await.map_err(|e| e.to_string())?;
            scratch.inspect(&scope).await.map_err(|e| e.to_string())?
        }
    };
    scratch.reset().await.map_err(|e| e.to_string())?;
    let after_model = normalize(&mut scratch, &after_ctx, &after_desired, &scope)
        .await
        .map_err(|e| format!("normalize after.hcl: {}", render(&e)))?;

    converge::<E>(&base, &scope, &mut target, &before_model, "before", None).await?;

    let renames = E::resolve_hints(&before_model, &after_model, &after_desired.hints);
    let forward = plan_between(&base, &scope, &before_model, &after_model, &renames, PlanMode::Generate, false);
    if let Some(expected) = &options.error {
        return match forward {
            Err(e) if error_kind(&e) == expected => Ok(()),
            Err(e) => Err(format!("expected a `{expected}` error, got: {e}")),
            Ok(plan) => Err(format!("expected a `{expected}` error, got a plan:\n{}", plan.to_sql())),
        };
    }
    let forward = forward.map_err(|e| format!("plan before -> after: {e}"))?;
    let sql = forward.to_sql();
    let expected_path = case.dir.join("expected.sql");
    match std::fs::read_to_string(&expected_path) {
        Ok(expected) if !bless() => {
            if expected != sql {
                return Err(format!(
                    "plan before -> after differs from expected.sql (re-run with OPENMIG_BLESS=1 to accept):\n{}",
                    text_diff(&expected, &sql)
                ));
            }
        }
        _ => {
            std::fs::write(&expected_path, &sql).map_err(|e| e.to_string())?;
        }
    }
    apply_plan::<E>(&mut target, &forward)
        .await
        .map_err(|e| format!("apply before -> after:\n{}\n{}", render(&e), sql))?;
    converge::<E>(&base, &scope, &mut target, &after_model, "after", Some(&forward)).await?;

    if options.reverse.unwrap_or(true) {
        let current = target.inspect(&scope).await.map_err(|e| e.to_string())?;
        let back = plan_between(&base, &scope, &current, &before_model, &RenameSet::default(), PlanMode::Generate, false)
            .map_err(|e| format!("plan after -> before: {e}"))?;
        apply_plan::<E>(&mut target, &back)
            .await
            .map_err(|e| format!("apply after -> before:\n{}\n{}", render(&e), back.to_sql()))?;
        converge::<E>(&base, &scope, &mut target, &before_model, "before (reverse)", Some(&back)).await?;
    }

    if options.roundtrip.unwrap_or(true) {
        let files = E::encode(&after_model, &base.settings, &EncodeOptions::default());
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut hcl = String::new();
        for f in &files {
            std::fs::write(dir.path().join(&f.path), &f.content).map_err(|e| e.to_string())?;
            hcl.push_str(&f.content);
        }
        let mut ctx = base.clone();
        ctx.schema_paths = vec![dir.path().to_path_buf()];
        let desired = load_desired(&ctx, &ui).map_err(|e| format!("load inspect output: {}\n{hcl}", render(&e)))?;
        scratch.reset().await.map_err(|e| e.to_string())?;
        let again = normalize(&mut scratch, &ctx, &desired, &scope)
            .await
            .map_err(|e| format!("normalize inspect output: {}\n{hcl}", render(&e)))?;
        let plan = plan_between(&base, &scope, &again, &after_model, &RenameSet::default(), PlanMode::Generate, false)
            .map_err(|e| e.to_string())?;
        if !plan.is_empty() {
            return Err(format!(
                "inspect output does not round-trip; remaining plan:\n{}\ninspect output:\n{hcl}",
                plan.to_sql()
            ));
        }
    }
    Ok(())
}

async fn converge<E: Engine>(
    ctx: &Context<E>,
    scope: &Scope,
    target: &mut E::Session,
    desired: &E::Model,
    label: &str,
    applied: Option<&openmig_core::Plan>,
) -> Result<(), String> {
    let current = target.inspect(scope).await.map_err(|e| e.to_string())?;
    let plan = plan_between(ctx, scope, &current, desired, &RenameSet::default(), PlanMode::Sync, false)
        .map_err(|e| format!("plan to {label}: {e}"))?;
    if plan.is_empty() {
        return Ok(());
    }
    if applied.is_none() {
        apply_plan::<E>(target, &plan)
            .await
            .map_err(|e| format!("apply empty -> {label}:\n{}\n{}", render(&e), plan.to_sql()))?;
        let current = target.inspect(scope).await.map_err(|e| e.to_string())?;
        let again = plan_between(ctx, scope, &current, desired, &RenameSet::default(), PlanMode::Sync, false)
            .map_err(|e| e.to_string())?;
        if again.is_empty() {
            return Ok(());
        }
        return Err(format!(
            "target does not converge to {label}; still pending after applying:\n{}",
            again.to_sql()
        ));
    }
    Err(format!(
        "target does not converge to {label}; still pending:\n{}\nafter applying:\n{}",
        plan.to_sql(),
        applied.map(|p| p.to_sql()).unwrap_or_default()
    ))
}

fn merge_scope(into: &mut Scope, other: Scope) {
    for r in other.declared_roles {
        if !into.declared_roles.contains(&r) {
            into.declared_roles.push(r);
        }
    }
    for t in other.data_tables {
        if !into.data_tables.contains(&t) {
            into.data_tables.push(t);
        }
    }
}

fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::Unsupported { .. } => "unsupported",
        Error::Schema { .. } => "schema",
        Error::DestructiveRefused => "destructive",
        _ => "other",
    }
}

fn render(e: &Error) -> String {
    match e {
        Error::Schema { rendered } => rendered.clone(),
        _ => e.to_string(),
    }
}

fn text_diff(expected: &str, actual: &str) -> String {
    let diff = similar::TextDiff::from_lines(expected, actual);
    diff.unified_diff()
        .context_radius(3)
        .header("expected.sql", "actual")
        .to_string()
}

/// Run every case of `kind` with libtest-mimic.
pub fn main<E: Engine>(kind: Kind) {
    let args = libtest_mimic::Arguments::from_args();
    let trials = discover(kind)
        .into_iter()
        .map(|case| {
            libtest_mimic::Trial::test(case.name.clone(), move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(run_case::<E>(kind, &case)).map_err(Into::into)
            })
        })
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
