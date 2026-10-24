//! PostgreSQL engine for OpenMig.
//!
//! * [`model`]: the schema model shared by decoding, inspection, diffing and planning.
//! * [`PgSession`]: connections, inspection, execution, the history table and the migration lock.
//! * [`PostgresEngine`]: the [`Engine`] implementation.

mod conn;
pub mod decode;
pub mod diff;
pub mod encode;
mod history;
mod inspect;
pub mod model;
pub mod restrict;
mod session;
mod settings;
pub mod spec;

pub use diff::Change;
pub use model::Database;
pub use session::{MIN_SERVER_VERSION, PgSession, SCRATCH_DATABASE};
pub use settings::{PgSettings, PublicDefaults};

use openmig_core::{
    DataTable, Desired, Dialect, DiffContext, EncodeOptions, EncodedFile, Engine, Error, Plan,
    RenameCandidate, RenameHints, RenameSet, Scope, SessionPurpose,
};
use openmig_hcl::{Document, SchemaSpec};
use std::time::Duration;

pub struct PostgresEngine;

impl Engine for PostgresEngine {
    const NAME: &'static str = "postgres";
    const DIALECT: Dialect = Dialect::Postgres;

    type Settings = PgSettings;
    type Model = Database;
    type Change = Change;
    type Session = PgSession;

    fn schema_spec(settings: &PgSettings) -> SchemaSpec {
        spec::schema_spec(settings)
    }

    fn decode(doc: &Document, settings: &PgSettings) -> Result<Desired<Database>, Error> {
        decode::decode(doc, settings)
    }

    fn encode(
        model: &Database,
        settings: &PgSettings,
        options: &EncodeOptions,
    ) -> Vec<EncodedFile> {
        encode::encode(model, settings, options)
    }

    fn restrict(model: &mut Database, scope: &Scope) {
        restrict::restrict(model, scope);
    }

    fn declared_roles(model: &Database) -> Vec<String> {
        model.roles.keys().cloned().collect()
    }

    fn data_tables(model: &Database) -> Vec<DataTable> {
        model
            .data
            .iter()
            .map(|(t, data)| {
                let mut columns: Vec<String> = data.key.clone();
                for row in data.rows.values() {
                    for c in row.keys() {
                        if !columns.contains(c) {
                            columns.push(c.clone());
                        }
                    }
                }
                DataTable {
                    path: vec![t.schema.clone(), t.name.clone()],
                    columns,
                    mode: data.mode,
                }
            })
            .collect()
    }

    fn is_empty(model: &Database) -> bool {
        model.is_empty()
    }

    fn resolve_hints(_current: &Database, _desired: &Database, _hints: &RenameHints) -> RenameSet {
        unimplemented!("postgres rename hints")
    }

    fn rename_candidates(_current: &Database, _desired: &Database) -> Vec<RenameCandidate> {
        unimplemented!("postgres rename candidates")
    }

    fn diff(
        _current: &Database,
        _desired: &Database,
        _ctx: &DiffContext<'_, Self>,
    ) -> Result<Vec<Change>, Error> {
        unimplemented!("postgres diff")
    }

    fn plan(
        _changes: Vec<Change>,
        _current: &Database,
        _desired: &Database,
        _ctx: &DiffContext<'_, Self>,
    ) -> Result<Plan, Error> {
        unimplemented!("postgres plan")
    }

    fn lock_timeout_sql(timeout: Duration) -> Option<String> {
        Some(format!(
            "SET LOCAL lock_timeout = '{}ms'",
            timeout.as_millis()
        ))
    }

    fn statement_timeout_sql(timeout: Duration) -> Option<String> {
        Some(format!(
            "SET statement_timeout = '{}ms'",
            timeout.as_millis()
        ))
    }

    fn scratch_spec(settings: &PgSettings, version: Option<u32>) -> openmig_scratch::Spec {
        openmig_scratch::Spec::Postgres(openmig_scratch::PgSpec {
            version,
            ..settings.scratch.clone()
        })
    }

    async fn connect(
        url: &str,
        settings: &PgSettings,
        purpose: SessionPurpose,
    ) -> Result<PgSession, Error> {
        PgSession::connect(url, settings, purpose).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_and_scratch() {
        assert_eq!(
            PostgresEngine::lock_timeout_sql(Duration::from_secs(5)).as_deref(),
            Some("SET LOCAL lock_timeout = '5000ms'")
        );
        assert_eq!(
            PostgresEngine::statement_timeout_sql(Duration::from_millis(1500)).as_deref(),
            Some("SET statement_timeout = '1500ms'")
        );
        let spec = PostgresEngine::scratch_spec(&PgSettings::default(), Some(17));
        let openmig_scratch::Spec::Postgres(pg) = spec else {
            panic!("expected a postgres spec");
        };
        assert_eq!(pg.version, Some(17));
    }
}
