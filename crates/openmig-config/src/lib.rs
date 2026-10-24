//! `openmig.toml`: named databases with flat keys and lazy `${ENV}` expansion.
//!
//! The common keys of a database table are parsed here; everything else is kept in
//! [`DatabaseConfig::engine_settings`] and deserialized by the engine (`Engine::Settings`), so this
//! crate does not depend on any engine.

mod env;
mod model;

pub use env::{EnvError, Expand, expand};
pub use model::{
    CONFIG_FILE_NAME, Config, ConfigError, DataMode, DatabaseConfig, DeploySettings, discover,
    load_file, parse,
};
