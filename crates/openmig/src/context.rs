//! Resolution of the config file, CLI overrides and engine settings into what a flow needs.

use openmig_config::DatabaseConfig;
use openmig_core::{Engine, Error};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Postgres,
    ClickHouse,
}

/// Everything a flow needs about one database, with CLI overrides applied.
#[derive(Debug, Clone)]
pub struct Context<E: Engine> {
    pub name: String,
    pub config: DatabaseConfig,
    pub settings: E::Settings,
    pub schema_paths: Vec<PathBuf>,
    pub migrations: Option<PathBuf>,
    /// Resolved lazily: `None` when neither config nor CLI provide a URL.
    pub url: Option<String>,
    pub vars: Vec<(String, String)>,
}

impl<E: Engine> Context<E> {
    pub fn url(&self) -> Result<&str, Error> {
        self.url.as_deref().ok_or_else(|| {
            Error::Config(format!(
                "database `{}` has no url; set `url` in openmig.toml or pass --url",
                self.name
            ))
        })
    }
}
