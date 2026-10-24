use crate::model::QualifiedName;
use openmig_core::Error;
use openmig_core::sql::{Dialect, quote_ident};
use openmig_scratch::PgSpec;
use serde::Deserialize;

/// PostgreSQL settings from a database's table in `openmig.toml` (after the common keys are
/// removed).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PgSettings {
    /// Schema for unqualified HCL labels.
    pub default_schema: String,
    /// `search_path` of sessions that apply SQL (scratch apply, migration files).
    pub search_path: Vec<String>,
    /// `schema.table` of the migration history table. Its schema is reserved for OpenMig and
    /// never inspected.
    pub history_table: String,
    pub public_defaults: PublicDefaults,
    /// Build and drop indexes on existing tables `CONCURRENTLY`.
    pub concurrent_indexes: bool,
    pub scratch: PgSpec,
}

impl Default for PgSettings {
    fn default() -> Self {
        Self {
            default_schema: "public".into(),
            search_path: vec!["$user".into(), "public".into()],
            history_table: "openmig.history".into(),
            public_defaults: PublicDefaults::Keep,
            concurrent_indexes: false,
            scratch: PgSpec::default(),
        }
    }
}

/// Starting point for `PUBLIC` privileges of declared objects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicDefaults {
    /// PostgreSQL's built-in defaults (`EXECUTE` on functions, `USAGE` on types, ...).
    #[default]
    Keep,
    /// No `PUBLIC` privileges unless granted.
    Revoke,
}

impl PgSettings {
    /// The history table as schema and name. An unqualified name lives in schema `openmig`.
    pub fn history(&self) -> Result<QualifiedName, Error> {
        let (schema, name) = match self.history_table.split_once('.') {
            Some((s, n)) => (s, n),
            None => ("openmig", self.history_table.as_str()),
        };
        if schema.is_empty() || name.is_empty() {
            return Err(Error::Config(format!(
                "invalid history_table `{}`: expected `schema.table`",
                self.history_table
            )));
        }
        if schema == self.default_schema || schema == "public" || schema.starts_with("pg_") {
            return Err(Error::Config(format!(
                "history_table `{}` must be in a schema of its own: the history schema is never \
                 inspected",
                self.history_table
            )));
        }
        Ok(QualifiedName::new(schema, name))
    }

    /// `search_path` as a SQL value: `"$user", "public"`.
    pub fn search_path_sql(&self) -> String {
        if self.search_path.is_empty() {
            return "''".into();
        }
        self.search_path
            .iter()
            .map(|s| quote_ident(s, Dialect::Postgres))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_parsing() {
        let s: PgSettings = toml_like(
            r#"{"default_schema": "app", "public_defaults": "revoke", "scratch": {"run_as": "pg"}}"#,
        );
        assert_eq!(s.default_schema, "app");
        assert_eq!(s.public_defaults, PublicDefaults::Revoke);
        assert_eq!(s.scratch.run_as.as_deref(), Some("pg"));
        assert_eq!(s.history_table, "openmig.history");
        assert_eq!(s.search_path_sql(), r#""$user", "public""#);
        assert_eq!(
            s.history().unwrap(),
            QualifiedName::new("openmig", "history")
        );
        assert!(serde_json::from_str::<PgSettings>(r#"{"bogus": 1}"#).is_err());
    }

    #[test]
    fn history_schema_must_be_dedicated() {
        let s = PgSettings {
            history_table: "public.history".into(),
            ..PgSettings::default()
        };
        assert!(s.history().is_err());
        let s = PgSettings {
            history_table: "migrations".into(),
            ..PgSettings::default()
        };
        assert_eq!(
            s.history().unwrap(),
            QualifiedName::new("openmig", "migrations")
        );
    }

    fn toml_like(json: &str) -> PgSettings {
        serde_json::from_str(json).unwrap()
    }
}
