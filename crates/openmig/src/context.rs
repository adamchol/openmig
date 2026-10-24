//! Resolution of the config file, CLI overrides and engine settings into what a flow needs.

use crate::cli::{EngineKind as CliEngine, GlobalArgs};
use indexmap::IndexMap;
use openmig_config::{Config, ConfigError, DatabaseConfig};
use openmig_core::{Engine, Error, ExcludeSet, SchemaFilter, Scope};
use openmig_hcl::{Number, Value, VarValue};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Postgres,
    ClickHouse,
}

impl EngineKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "postgres" => Some(EngineKind::Postgres),
            "clickhouse" => Some(EngineKind::ClickHouse),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Postgres => "postgres",
            EngineKind::ClickHouse => "clickhouse",
        }
    }
}

impl From<CliEngine> for EngineKind {
    fn from(e: CliEngine) -> Self {
        match e {
            CliEngine::Postgres => EngineKind::Postgres,
            CliEngine::Clickhouse => EngineKind::ClickHouse,
        }
    }
}

fn config_error(e: ConfigError) -> Error {
    Error::Config(e.to_string())
}

/// Load the config file, or build a config-less one from `--engine`/`--migrations`/`--url`.
pub fn load_config(global: &GlobalArgs) -> Result<Config, Error> {
    let path = match &global.config {
        Some(p) => Some(p.clone()),
        None => {
            let cwd = std::env::current_dir().map_err(|e| Error::io("current directory", e))?;
            match openmig_config::discover(&cwd) {
                Ok(p) => Some(p),
                Err(ConfigError::NotFound(_)) if global.engine.is_some() => None,
                Err(e) => return Err(config_error(e)),
            }
        }
    };
    match path {
        Some(p) => openmig_config::load_file(&p).map_err(config_error),
        None => {
            let engine = EngineKind::from(global.engine.expect("checked above"));
            let mut databases = IndexMap::new();
            databases.insert(
                engine.as_str().to_owned(),
                DatabaseConfig::bare(engine.as_str(), engine.as_str()),
            );
            Ok(Config {
                path: None,
                root: std::env::current_dir().map_err(|e| Error::io("current directory", e))?,
                databases,
            })
        }
    }
}

/// The databases a command runs for: `--all`, `--db NAME`, or the only one.
pub fn select<'c>(
    config: &'c Config,
    global: &GlobalArgs,
) -> Result<Vec<&'c DatabaseConfig>, Error> {
    if global.all {
        if config.databases.is_empty() {
            return Err(Error::Config("the config defines no databases".into()));
        }
        return Ok(config.databases.values().collect());
    }
    Ok(vec![
        config
            .database(global.db.as_deref())
            .map_err(config_error)?,
    ])
}

/// Everything a flow needs about one database, with CLI overrides applied.
#[derive(Debug)]
pub struct Context<E: Engine> {
    pub name: String,
    pub config: DatabaseConfig,
    pub settings: E::Settings,
    pub schema_paths: Vec<PathBuf>,
    pub migrations: Option<PathBuf>,
    url_override: Option<String>,
    pub vars: IndexMap<String, VarValue>,
}

impl<E: Engine> Clone for Context<E> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            config: self.config.clone(),
            settings: self.settings.clone(),
            schema_paths: self.schema_paths.clone(),
            migrations: self.migrations.clone(),
            url_override: self.url_override.clone(),
            vars: self.vars.clone(),
        }
    }
}

impl<E: Engine> Context<E> {
    pub fn new(db: &DatabaseConfig, global: &GlobalArgs) -> Result<Self, Error> {
        let settings = E::Settings::deserialize(toml::Value::Table(db.engine_settings.clone()))
            .map_err(|e| Error::Config(format!("[databases.{}]: {}", db.name, e.message())))?;
        let cwd = std::env::current_dir().map_err(|e| Error::io("current directory", e))?;
        let absolute = |p: &PathBuf| {
            if p.is_absolute() {
                p.clone()
            } else {
                cwd.join(p)
            }
        };
        let mut schema_paths = db.schema.clone();
        schema_paths.extend(global.schema.iter().map(absolute));
        let migrations = global
            .migrations
            .as_ref()
            .map(absolute)
            .or_else(|| db.migrations.clone());
        let mut vars = IndexMap::new();
        for (k, v) in &db.vars {
            vars.insert(k.clone(), VarValue::Value(toml_to_value(v)));
        }
        for (k, v) in &global.vars {
            vars.insert(k.clone(), VarValue::Raw(v.clone()));
        }
        Ok(Self {
            name: db.name.clone(),
            config: db.clone(),
            settings,
            schema_paths,
            migrations,
            url_override: global.url.clone(),
            vars,
        })
    }

    /// The target database URL: `--url`, else the config's `url` with `${ENV}` expanded.
    pub fn url(&self) -> Result<String, Error> {
        if let Some(u) = &self.url_override {
            return Ok(u.clone());
        }
        let url = self.config.url.as_ref().ok_or_else(|| {
            Error::Config(format!(
                "database `{}` has no url; set `url` in openmig.toml or pass --url",
                self.name
            ))
        })?;
        url.resolve()
            .map_err(|e| Error::Config(format!("[databases.{}] url: {e}", self.name)))
    }

    pub fn migrations_dir(&self) -> Result<&PathBuf, Error> {
        self.migrations.as_ref().ok_or_else(|| {
            Error::Config(format!(
                "database `{}` has no migrations directory; set `migrations` in openmig.toml or pass --migrations",
                self.name
            ))
        })
    }

    pub fn require_schema(&self) -> Result<&[PathBuf], Error> {
        if self.schema_paths.is_empty() {
            return Err(Error::Config(format!(
                "database `{}` has no schema; set `schema` in openmig.toml or pass --schema",
                self.name
            )));
        }
        Ok(&self.schema_paths)
    }

    /// The inspection and planning scope from config (roles and data tables are filled in from
    /// the desired model by the flows).
    pub fn scope(&self) -> Result<Scope, Error> {
        let exclude = ExcludeSet::new(&self.config.exclude)
            .map_err(|e| Error::Config(format!("[databases.{}] exclude: {e}", self.name)))?;
        Ok(Scope {
            schemas: match &self.config.schemas {
                None => SchemaFilter::All,
                Some(list) => SchemaFilter::Only(list.clone()),
            },
            exclude,
            manage_roles: self.config.manage_roles,
            declared_roles: Vec::new(),
            data_tables: Vec::new(),
            data_max_rows: self.config.data_max_rows,
        })
    }
}

fn toml_to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number(Number::Int(*i)),
        toml::Value::Float(f) => Value::Number(Number::Float(*f)),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::List(items.iter().map(toml_to_value).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_value(v)))
                .collect(),
        ),
    }
}
