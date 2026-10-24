use crate::model::QualifiedName;
use openmig_core::Error;
use openmig_core::sql::{Dialect, quote_ident};
use openmig_scratch::ChSpec;
use serde::Deserialize;

/// Databases that belong to the server and are never inspected or managed.
pub const SYSTEM_DATABASES: &[&str] = &["system", "information_schema", "INFORMATION_SCHEMA"];

/// ClickHouse settings from a database's table in `openmig.toml` (after the common keys are
/// removed).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChSettings {
    /// Database for unqualified HCL labels.
    pub default_database: String,
    /// Cluster name or macro (`{cluster}`). When set, DDL for databases, access entities and
    /// tables outside `Replicated` databases runs `ON CLUSTER`, and the history table is
    /// replicated.
    pub cluster: Option<String>,
    /// `database.table` of the migration history table. Its database is reserved for OpenMig and
    /// never inspected.
    pub history_table: String,
    pub scratch: ChSpec,
}

impl Default for ChSettings {
    fn default() -> Self {
        Self {
            default_database: "default".into(),
            cluster: None,
            history_table: "openmig.history".into(),
            scratch: ChSpec::default(),
        }
    }
}

impl ChSettings {
    /// The history table as database and name. An unqualified name lives in database `openmig`.
    pub fn history(&self) -> Result<QualifiedName, Error> {
        let (database, name) = match self.history_table.split_once('.') {
            Some((d, n)) => (d, n),
            None => ("openmig", self.history_table.as_str()),
        };
        if database.is_empty() || name.is_empty() {
            return Err(Error::Config(format!(
                "invalid history_table `{}`: expected `database.table`",
                self.history_table
            )));
        }
        if database == self.default_database
            || database == "default"
            || SYSTEM_DATABASES.contains(&database)
        {
            return Err(Error::Config(format!(
                "history_table `{}` must be in a database of its own: the history database is \
                 never inspected",
                self.history_table
            )));
        }
        Ok(QualifiedName::new(database, name))
    }

    /// ` ON CLUSTER `name`` when a cluster is configured, else an empty string.
    pub fn on_cluster(&self) -> String {
        match &self.cluster {
            Some(cluster) => format!(" ON CLUSTER {}", quote_ident(cluster, Dialect::ClickHouse)),
            None => String::new(),
        }
    }

    /// The macro name when `cluster` is a macro reference such as `{cluster}`.
    pub fn cluster_macro(&self) -> Option<&str> {
        self.cluster
            .as_deref()?
            .strip_prefix('{')?
            .strip_suffix('}')
            .filter(|name| !name.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_parsing() {
        let settings: ChSettings = toml::from_str(
            r#"
            cluster = "{cluster}"
            history_table = "ops.migrations"
            [scratch]
            storage_policies = ["encrypted"]
            "#,
        )
        .unwrap();
        assert_eq!(settings.default_database, "default");
        assert_eq!(settings.cluster_macro(), Some("cluster"));
        assert_eq!(settings.on_cluster(), " ON CLUSTER `{cluster}`");
        assert_eq!(
            settings.history().unwrap(),
            QualifiedName::new("ops", "migrations")
        );
        assert_eq!(settings.scratch.storage_policies, vec!["encrypted"]);

        let defaults = ChSettings::default();
        assert_eq!(defaults.on_cluster(), "");
        assert_eq!(defaults.cluster_macro(), None);
        assert_eq!(
            defaults.history().unwrap(),
            QualifiedName::new("openmig", "history")
        );
        assert!(toml::from_str::<ChSettings>("unknown = 1").is_err());
    }

    #[test]
    fn history_must_have_its_own_database() {
        for table in ["default.history", "system.history", ".x", "x."] {
            let settings = ChSettings {
                history_table: table.into(),
                ..ChSettings::default()
            };
            assert!(settings.history().is_err(), "{table}");
        }
        let unqualified = ChSettings {
            history_table: "migrations".into(),
            ..ChSettings::default()
        };
        assert_eq!(
            unqualified.history().unwrap(),
            QualifiedName::new("openmig", "migrations")
        );
        let plain = ChSettings {
            cluster: Some("main".into()),
            ..ChSettings::default()
        };
        assert_eq!(plain.cluster_macro(), None);
        assert_eq!(plain.on_cluster(), " ON CLUSTER `main`");
    }
}
