use crate::env::Expand;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "openmig.toml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no {CONFIG_FILE_NAME} found in {0} or any parent directory (use --config)")]
    NotFound(PathBuf),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

/// How rows declared in `data` blocks are managed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataMode {
    /// Ignore `data` blocks.
    Off,
    /// Insert declared rows that are missing; never update or delete.
    Insert,
    /// Make the table contain exactly the declared rows.
    #[default]
    Sync,
}

/// `[databases.<name>.deploy]`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeploySettings {
    /// Lock timeout for each transactional migration file (`5s`).
    pub lock_timeout: String,
    /// Retries of a transactional file after a lock timeout.
    pub retries: u32,
    /// How long to wait for the migration lock.
    pub lock_wait: String,
    pub allow_out_of_order: bool,
}

impl Default for DeploySettings {
    fn default() -> Self {
        Self {
            lock_timeout: "5s".into(),
            retries: 5,
            lock_wait: "10m".into(),
            allow_out_of_order: false,
        }
    }
}

/// One `[databases.<name>]` table.
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    pub name: String,
    /// `postgres` or `clickhouse`.
    pub engine: String,
    /// Schema files or directories, absolute (resolved against the config file's directory).
    pub schema: Vec<PathBuf>,
    /// Migration directory, absolute.
    pub migrations: Option<PathBuf>,
    pub url: Option<Expand>,
    /// Server major version used for the scratch server and checked against targets.
    pub version: Option<u32>,
    /// `"*"` (default) or an explicit list of schemas/databases to manage.
    pub schemas: Option<Vec<String>>,
    pub exclude: Vec<String>,
    pub manage_roles: bool,
    pub data: DataMode,
    pub data_max_rows: u64,
    /// Values for schema `variable`s.
    pub vars: IndexMap<String, toml::Value>,
    pub deploy: DeploySettings,
    /// Every other key, for the engine's settings (`default_schema`, `cluster`, `scratch`, ...).
    pub engine_settings: toml::Table,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The config file, or `None` for config-less mode.
    pub path: Option<PathBuf>,
    /// Directory relative paths are resolved against.
    pub root: PathBuf,
    pub databases: IndexMap<String, DatabaseConfig>,
}

impl Config {
    /// Select a database by name, or the only one when `name` is `None`.
    pub fn database(&self, name: Option<&str>) -> Result<&DatabaseConfig, ConfigError> {
        let _ = name;
        unimplemented!("config database selection")
    }
}

/// Find `openmig.toml` in `start` or its parents.
pub fn discover(start: &Path) -> Result<PathBuf, ConfigError> {
    let _ = start;
    unimplemented!("config discovery")
}

pub fn load_file(path: &Path) -> Result<Config, ConfigError> {
    let _ = path;
    unimplemented!("config loading")
}

/// Parse config text; `root` is the directory relative paths are resolved against.
pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Config, ConfigError> {
    let _ = (text, path, root);
    unimplemented!("config parsing")
}
