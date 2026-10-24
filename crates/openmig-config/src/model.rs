use crate::env::Expand;
use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "openmig.toml";

pub const ENGINES: &[&str] = &["postgres", "clickhouse"];

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
    #[error("{0}")]
    Selection(String),
}

/// How rows declared in `data` blocks are managed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
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
    /// `None` for `"*"` (every non-system schema/database), or an explicit list.
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

impl DatabaseConfig {
    /// A database described only by CLI flags (config-less mode).
    pub fn bare(name: &str, engine: &str) -> Self {
        Self {
            name: name.to_owned(),
            engine: engine.to_owned(),
            schema: Vec::new(),
            migrations: None,
            url: None,
            version: None,
            schemas: None,
            exclude: Vec::new(),
            manage_roles: false,
            data: DataMode::default(),
            data_max_rows: 10_000,
            vars: IndexMap::new(),
            deploy: DeploySettings::default(),
            engine_settings: toml::Table::new(),
        }
    }
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
        let names = || {
            self.databases
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match name {
            Some(n) => self.databases.get(n).ok_or_else(|| {
                ConfigError::Selection(format!(
                    "no database `{n}` in {}; available: {}",
                    self.display_path(),
                    names()
                ))
            }),
            None => match self.databases.len() {
                1 => Ok(&self.databases[0]),
                0 => Err(ConfigError::Selection(format!(
                    "{} defines no databases",
                    self.display_path()
                ))),
                _ => Err(ConfigError::Selection(format!(
                    "{} defines several databases ({}); choose one with --db or use --all",
                    self.display_path(),
                    names()
                ))),
            },
        }
    }

    fn display_path(&self) -> String {
        self.path.as_ref().map_or_else(
            || "the command line".to_owned(),
            |p| p.display().to_string(),
        )
    }
}

/// Find `openmig.toml` in `start` or its parents.
pub fn discover(start: &Path) -> Result<PathBuf, ConfigError> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        dir = d.parent();
    }
    Err(ConfigError::NotFound(start.to_path_buf()))
}

pub fn load_file(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    };
    let root = std::path::absolute(&root).unwrap_or(root);
    parse(&text, path, &root)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    databases: IndexMap<String, toml::Table>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

/// Parse config text; `root` is the directory relative paths are resolved against.
pub fn parse(text: &str, path: &Path, root: &Path) -> Result<Config, ConfigError> {
    let invalid = |message: String| ConfigError::Invalid {
        path: path.to_path_buf(),
        message,
    };
    let raw: RawConfig = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let mut databases = IndexMap::new();
    for (name, table) in raw.databases {
        let db = parse_database(&name, table, root)
            .map_err(|m| invalid(format!("[databases.{name}]: {m}")))?;
        databases.insert(name, db);
    }
    Ok(Config {
        path: Some(path.to_path_buf()),
        root: root.to_path_buf(),
        databases,
    })
}

fn take<T: serde::de::DeserializeOwned>(
    table: &mut toml::Table,
    key: &str,
) -> Result<Option<T>, String> {
    match table.remove(key) {
        None => Ok(None),
        Some(v) => T::deserialize(v)
            .map(Some)
            .map_err(|e| format!("`{key}`: {}", e.message())),
    }
}

fn parse_database(name: &str, mut t: toml::Table, root: &Path) -> Result<DatabaseConfig, String> {
    let resolve = |p: String| -> PathBuf {
        let p = PathBuf::from(p);
        if p.is_absolute() { p } else { root.join(p) }
    };
    let engine: String = take(&mut t, "engine")?.ok_or("`engine` is required")?;
    if !ENGINES.contains(&engine.as_str()) {
        return Err(format!(
            "`engine` must be one of {}, found `{engine}`",
            ENGINES.join(", ")
        ));
    }
    let schema = take::<OneOrMany>(&mut t, "schema")?
        .map(OneOrMany::into_vec)
        .unwrap_or_default()
        .into_iter()
        .map(resolve)
        .collect();
    let migrations = take::<String>(&mut t, "migrations")?.map(resolve);
    let url = take::<Expand>(&mut t, "url")?;
    let version = take::<u32>(&mut t, "version")?;
    let schemas = match take::<OneOrMany>(&mut t, "schemas")? {
        None => None,
        Some(OneOrMany::One(s)) if s == "*" => None,
        Some(OneOrMany::One(s)) => {
            return Err(format!(
                "`schemas` must be \"*\" or a list of names, found \"{s}\""
            ));
        }
        Some(OneOrMany::Many(v)) => Some(v),
    };
    let exclude = take::<Vec<String>>(&mut t, "exclude")?.unwrap_or_default();
    let manage_roles = take::<bool>(&mut t, "manage_roles")?.unwrap_or(false);
    let data = take::<DataMode>(&mut t, "data")?.unwrap_or_default();
    let data_max_rows = take::<u64>(&mut t, "data_max_rows")?.unwrap_or(10_000);
    let vars = take::<IndexMap<String, toml::Value>>(&mut t, "vars")?.unwrap_or_default();
    let deploy = take::<DeploySettings>(&mut t, "deploy")?.unwrap_or_default();
    Ok(DatabaseConfig {
        name: name.to_owned(),
        engine,
        schema,
        migrations,
        url,
        version,
        schemas,
        exclude,
        manage_roles,
        data,
        data_max_rows,
        vars,
        deploy,
        engine_settings: t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn cfg(text: &str) -> Result<Config, ConfigError> {
        parse(text, Path::new("/p/openmig.toml"), Path::new("/p"))
    }

    #[test]
    fn parses_common_and_engine_keys() {
        let c = cfg(r#"
            [databases.app]
            engine = "postgres"
            schema = ["db/schema", "/abs/extra"]
            migrations = "db/migrations"
            url = "${DATABASE_URL}"
            version = 17
            exclude = ["role:cnpg_*"]
            manage_roles = true
            data = "insert"
            default_schema = "app"

            [databases.app.vars]
            tenant = "acme"

            [databases.app.deploy]
            lock_timeout = "10s"

            [databases.app.scratch]
            run_as = "postgres"

            [databases.analytics]
            engine = "clickhouse"
            schema = "ch"
            schemas = ["dialo", "otel"]
            cluster = "{cluster}"
        "#)
        .unwrap();
        let app = c.database(Some("app")).unwrap();
        assert_eq!(
            app.schema,
            vec![PathBuf::from("/p/db/schema"), PathBuf::from("/abs/extra")]
        );
        assert_eq!(app.migrations, Some(PathBuf::from("/p/db/migrations")));
        assert_eq!(app.url.as_ref().unwrap().raw(), "${DATABASE_URL}");
        assert_eq!(app.version, Some(17));
        assert_eq!(app.schemas, None);
        assert!(app.manage_roles);
        assert_eq!(app.data, DataMode::Insert);
        assert_eq!(app.deploy.lock_timeout, "10s");
        assert_eq!(app.deploy.retries, 5);
        assert_eq!(app.vars["tenant"].as_str(), Some("acme"));
        assert_eq!(
            app.engine_settings.keys().collect::<Vec<_>>(),
            vec!["default_schema", "scratch"]
        );
        let ch = c.database(Some("analytics")).unwrap();
        assert_eq!(ch.schema, vec![PathBuf::from("/p/ch")]);
        assert_eq!(ch.schemas, Some(vec!["dialo".into(), "otel".into()]));
        assert!(c.database(None).is_err());
    }

    #[test]
    fn errors_are_specific() {
        let e = cfg("[databases.x]\nschema = []").unwrap_err().to_string();
        assert!(e.contains("`engine` is required"), "{e}");
        let e = cfg("[databases.x]\nengine = \"mysql\"")
            .unwrap_err()
            .to_string();
        assert!(e.contains("must be one of postgres, clickhouse"), "{e}");
        let e = cfg("[databases.x]\nengine = \"postgres\"\nversion = \"17\"")
            .unwrap_err()
            .to_string();
        assert!(e.contains("`version`"), "{e}");
        let e = cfg("[databse.x]").unwrap_err().to_string();
        assert!(e.contains("unknown field"), "{e}");
        let e = cfg("[databases.x]\nengine = \"postgres\"\n[databases.x.deploy]\nretry = 1")
            .unwrap_err()
            .to_string();
        assert!(e.contains("retry"), "{e}");
    }

    #[test]
    fn single_database_is_default() {
        let c = cfg("[databases.only]\nengine = \"postgres\"").unwrap();
        assert_eq!(c.database(None).unwrap().name, "only");
    }

    #[test]
    fn discovers_upwards() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "").unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            discover(&nested).unwrap(),
            dir.path().join(CONFIG_FILE_NAME)
        );
    }
}
