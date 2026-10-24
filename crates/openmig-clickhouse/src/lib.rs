//! ClickHouse engine for OpenMig.
//!
//! * [`model`]: the schema model shared by decoding, inspection, diffing and planning.
//! * [`ddl`]: a parser for the canonical DDL the server reports.
//! * [`privileges`]: the server's privilege hierarchy and canonical privilege sets.
//! * [`ChSession`]: the HTTP connection, inspection, execution, the history table and the
//!   migration lock.
//! * [`ClickHouseEngine`]: the [`Engine`] implementation.

pub mod ddl;
pub mod diff;
mod history;
mod http;
mod inspect;
pub mod model;
pub mod privileges;
pub mod restrict;
mod session;
mod settings;

pub use diff::Change;
pub use model::Model;
pub use session::{ChSession, MIN_SERVER_VERSION};
pub use settings::{ChSettings, SYSTEM_DATABASES};

use openmig_core::{
    DataTable, Desired, Dialect, DiffContext, EncodeOptions, EncodedFile, Engine, Error, Plan,
    RenameCandidate, RenameHints, RenameSet, Scope, SessionPurpose,
};
use openmig_hcl::{Document, SchemaSpec};
use std::time::Duration;

/// Cluster name of scratch servers when the `cluster` setting is a macro.
const SCRATCH_CLUSTER: &str = "openmig";

pub struct ClickHouseEngine;

impl Engine for ClickHouseEngine {
    const NAME: &'static str = "clickhouse";
    const DIALECT: Dialect = Dialect::ClickHouse;

    type Settings = ChSettings;
    type Model = Model;
    type Change = Change;
    type Session = ChSession;

    fn schema_spec(_settings: &ChSettings) -> SchemaSpec {
        unimplemented!("clickhouse schema spec")
    }

    fn decode(_doc: &Document, _settings: &ChSettings) -> Result<Desired<Model>, Error> {
        unimplemented!("clickhouse decode")
    }

    fn encode(
        _model: &Model,
        _settings: &ChSettings,
        _options: &EncodeOptions,
    ) -> Vec<EncodedFile> {
        unimplemented!("clickhouse encode")
    }

    fn restrict(model: &mut Model, scope: &Scope) {
        restrict::restrict(model, scope);
    }

    /// Declared roles and users: both can receive grants.
    fn declared_roles(model: &Model) -> Vec<String> {
        model
            .roles
            .keys()
            .chain(model.users.keys())
            .cloned()
            .collect()
    }

    fn data_tables(_model: &Model) -> Vec<DataTable> {
        Vec::new()
    }

    fn is_empty(model: &Model) -> bool {
        model.is_empty()
    }

    fn resolve_hints(_current: &Model, _desired: &Model, _hints: &RenameHints) -> RenameSet {
        unimplemented!("clickhouse rename hints")
    }

    fn rename_candidates(_current: &Model, _desired: &Model) -> Vec<RenameCandidate> {
        unimplemented!("clickhouse rename candidates")
    }

    fn diff(
        _current: &Model,
        _desired: &Model,
        _ctx: &DiffContext<'_, Self>,
    ) -> Result<Vec<Change>, Error> {
        unimplemented!("clickhouse diff")
    }

    fn plan(
        _changes: Vec<Change>,
        _current: &Model,
        _desired: &Model,
        _ctx: &DiffContext<'_, Self>,
    ) -> Result<Plan, Error> {
        unimplemented!("clickhouse plan")
    }

    /// `max_execution_time` in whole seconds (rounded up); it lasts for the rest of the HTTP
    /// session, i.e. the current migration file.
    fn statement_timeout_sql(timeout: Duration) -> Option<String> {
        let seconds = timeout.as_millis().div_ceil(1000).max(1);
        Some(format!("SET max_execution_time = {seconds}"))
    }

    /// The scratch server: the `scratch` settings, with the cluster that `cluster` names. A
    /// macro (`{cluster}`) becomes a macro of the scratch server naming its single-node cluster
    /// (`openmig` unless the scratch settings choose); a plain name becomes the cluster's name.
    fn scratch_spec(settings: &ChSettings, version: Option<u32>) -> openmig_scratch::Spec {
        let mut spec = settings.scratch.clone();
        spec.version = version;
        if let Some(name) = settings.cluster_macro() {
            let cluster = spec
                .macros
                .get(name)
                .cloned()
                .or_else(|| spec.cluster.clone())
                .unwrap_or_else(|| SCRATCH_CLUSTER.to_owned());
            spec.macros.insert(name.to_owned(), cluster.clone());
            spec.cluster = Some(cluster);
        } else if let Some(cluster) = &settings.cluster {
            spec.cluster = Some(cluster.clone());
        }
        openmig_scratch::Spec::ClickHouse(spec)
    }

    async fn connect(
        url: &str,
        settings: &ChSettings,
        purpose: SessionPurpose,
    ) -> Result<ChSession, Error> {
        ChSession::connect(url, settings, purpose).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec(settings: &ChSettings) -> openmig_scratch::ChSpec {
        match ClickHouseEngine::scratch_spec(settings, Some(26)) {
            openmig_scratch::Spec::ClickHouse(spec) => spec,
            openmig_scratch::Spec::Postgres(_) => panic!("expected a clickhouse spec"),
        }
    }

    #[test]
    fn scratch_clusters() {
        let plain = spec(&ChSettings::default());
        assert_eq!(plain.version, Some(26));
        assert_eq!(plain.cluster, None);

        let named = spec(&ChSettings {
            cluster: Some("main".into()),
            ..ChSettings::default()
        });
        assert_eq!(named.cluster.as_deref(), Some("main"));

        let mut settings = ChSettings {
            cluster: Some("{cluster}".into()),
            ..ChSettings::default()
        };
        settings.scratch.storage_policies = vec!["encrypted".into()];
        let macro_spec = spec(&settings);
        assert_eq!(macro_spec.cluster.as_deref(), Some("openmig"));
        assert_eq!(macro_spec.macros["cluster"], "openmig");
        assert_eq!(macro_spec.storage_policies, vec!["encrypted"]);

        settings.cluster = Some("{ch_cluster}".into());
        settings.scratch.macros = BTreeMap::from([("ch_cluster".into(), "wax".into())]);
        let custom = spec(&settings);
        assert_eq!(custom.cluster.as_deref(), Some("wax"));
        assert_eq!(custom.macros["ch_cluster"], "wax");
    }

    #[test]
    fn timeouts() {
        assert_eq!(
            ClickHouseEngine::statement_timeout_sql(Duration::from_millis(1500)).as_deref(),
            Some("SET max_execution_time = 2")
        );
        assert_eq!(
            ClickHouseEngine::lock_timeout_sql(Duration::from_secs(5)),
            None
        );
    }
}
