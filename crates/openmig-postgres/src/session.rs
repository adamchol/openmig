//! An open connection to a PostgreSQL database.

use crate::PostgresEngine;
use crate::conn::{ConnectSpec, exec_error};
use crate::history;
use crate::inspect::{self, InspectContext};
use crate::model::{Database, QualifiedName};
use crate::settings::PgSettings;
use openmig_core::sql::{Dialect, quote_ident};
use openmig_core::{
    Capabilities, Error, ExecError, HistoryRow, Scope, ServerVersion, Session, SessionPurpose,
};
use std::time::{Duration, Instant};
use tokio_postgres::Client;

/// Lowest supported server major version.
pub const MIN_SERVER_VERSION: u32 = 14;

const APPLICATION_NAME: &str = "openmig";
/// Database used for administrative connections of scratch sessions.
const MAINTENANCE_DATABASE: &str = "postgres";
/// Working database of scratch sessions whose URL names no other database.
pub const SCRATCH_DATABASE: &str = "openmig_scratch";
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Settings that make catalog output deterministic. Applied for the duration of
/// [`Session::inspect`] and restored afterwards, whatever the session's purpose.
const INSPECTION_SETTINGS: &[(&str, &str)] = &[
    ("search_path", ""),
    ("TimeZone", "UTC"),
    ("DateStyle", "ISO"),
    ("IntervalStyle", "postgres"),
    ("extra_float_digits", "3"),
    ("standard_conforming_strings", "on"),
    ("quote_all_identifiers", "off"),
    ("bytea_output", "hex"),
    ("jit", "off"),
];

/// A PostgreSQL session.
///
/// Statements run with the configured `search_path` (`PgSettings::search_path`); inspection
/// switches to an empty `search_path` and fixed output settings, then restores the previous
/// values. Target and scratch sessions also use UTC, ISO dates, `postgres` intervals and
/// `extra_float_digits = 3` for statements.
///
/// A scratch session works in its own database: the database named in the URL, or
/// [`SCRATCH_DATABASE`] when the URL names `postgres` or none. Connecting (re)creates it empty from
/// `template0`; [`Session::reset`] drops it, drops every role except the bootstrap superuser,
/// built-in roles and the connecting user, and creates it again.
pub struct PgSession {
    client: Client,
    spec: ConnectSpec,
    settings: PgSettings,
    purpose: SessionPurpose,
    version: ServerVersion,
    history: QualifiedName,
    current_user: String,
}

impl PgSession {
    pub async fn connect(
        url: &str,
        settings: &PgSettings,
        purpose: SessionPurpose,
    ) -> Result<Self, Error> {
        let spec = ConnectSpec::parse(url)?;
        let history = settings.history()?;
        let spec = if purpose == SessionPurpose::Scratch {
            let work = scratch_database(&spec);
            let (admin, _, _) = connect_checked(&spec.with_dbname(MAINTENANCE_DATABASE)).await?;
            recreate_database(&admin, &work).await?;
            spec.with_dbname(&work)
        } else {
            spec
        };
        let (client, version, current_user) = connect_checked(&spec).await?;
        let session = Self {
            client,
            spec,
            settings: settings.clone(),
            purpose,
            version,
            history,
            current_user,
        };
        session.apply_session_settings().await?;
        Ok(session)
    }

    /// The underlying client, for queries the trait does not cover.
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn purpose(&self) -> SessionPurpose {
        self.purpose
    }

    /// The database this session is connected to.
    pub fn database(&self) -> &str {
        self.spec.dbname().unwrap_or_default()
    }

    /// The role this session is connected as.
    pub fn current_user(&self) -> &str {
        &self.current_user
    }

    fn session_settings_sql(&self) -> String {
        let mut sql = format!("SET search_path = {}", self.settings.search_path_sql());
        if self.purpose != SessionPurpose::Deploy {
            sql.push_str(
                "; SET TimeZone = 'UTC'; SET DateStyle = 'ISO'; SET IntervalStyle = 'postgres'; \
                 SET extra_float_digits = 3",
            );
        }
        sql
    }

    async fn apply_session_settings(&self) -> Result<(), ExecError> {
        let sql = self.session_settings_sql();
        self.client
            .batch_execute(&sql)
            .await
            .map_err(|e| exec_error(&e).with_statement(sql))
    }

    /// Switch to [`INSPECTION_SETTINGS`], returning the previous values.
    async fn enter_inspection(&self) -> Result<Vec<(String, String)>, Error> {
        let (names, values): (Vec<&str>, Vec<&str>) = INSPECTION_SETTINGS.iter().copied().unzip();
        let rows = self
            .client
            .query(
                "SELECT s.name, o.old, pg_catalog.set_config(s.name, s.value, false) \
                 FROM unnest($1::text[], $2::text[]) WITH ORDINALITY AS s(name, value, n), \
                 LATERAL (SELECT pg_catalog.current_setting(s.name) AS old OFFSET 0) o \
                 ORDER BY s.n",
                &[&names, &values],
            )
            .await
            .map_err(|e| exec_error(&e))?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    async fn leave_inspection(&self, saved: Vec<(String, String)>) -> Result<(), Error> {
        let (names, values): (Vec<String>, Vec<String>) = saved.into_iter().unzip();
        self.client
            .query(
                "SELECT pg_catalog.set_config(s.name, s.value, false) \
                 FROM unnest($1::text[], $2::text[]) AS s(name, value)",
                &[&names, &values],
            )
            .await
            .map_err(|e| exec_error(&e))?;
        Ok(())
    }

    fn lock_key_sql() -> &'static str {
        "pg_catalog.hashtext('openmig:' || pg_catalog.current_database())"
    }
}

/// The working database of a scratch session.
fn scratch_database(spec: &ConnectSpec) -> String {
    match spec.dbname() {
        Some(db) if db != MAINTENANCE_DATABASE && db != "template1" && !db.is_empty() => {
            db.to_owned()
        }
        _ => SCRATCH_DATABASE.to_owned(),
    }
}

async fn connect_checked(spec: &ConnectSpec) -> Result<(Client, ServerVersion, String), Error> {
    let client = spec.connect(APPLICATION_NAME).await?;
    let row = client
        .query_one(
            "SELECT pg_catalog.current_setting('server_version_num')::int4, \
             pg_catalog.current_setting('server_version'), current_user::text",
            &[],
        )
        .await
        .map_err(|e| Error::Connect {
            target: spec.display.clone(),
            message: crate::conn::describe(&e),
        })?;
    let num: i32 = row.get(0);
    let text: String = row.get(1);
    let num = u32::try_from(num).unwrap_or_default();
    let version = ServerVersion {
        major: num / 10000,
        minor: num % 10000,
        text: text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned(),
    };
    if version.major < MIN_SERVER_VERSION {
        return Err(Error::Connect {
            target: spec.display.clone(),
            message: format!(
                "PostgreSQL {} is not supported (supported: {MIN_SERVER_VERSION} and newer)",
                version.text
            ),
        });
    }
    Ok((client, version, row.get(2)))
}

fn ident(name: &str) -> String {
    quote_ident(name, Dialect::Postgres)
}

async fn admin_execute(client: &Client, sql: &str) -> Result<(), Error> {
    client
        .batch_execute(sql)
        .await
        .map_err(|e| Error::Exec(exec_error(&e).with_statement(sql)))
}

async fn recreate_database(admin: &Client, name: &str) -> Result<(), Error> {
    admin_execute(
        admin,
        &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", ident(name)),
    )
    .await?;
    admin_execute(
        admin,
        &format!("CREATE DATABASE {} TEMPLATE template0", ident(name)),
    )
    .await
}

impl Session<PostgresEngine> for PgSession {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactional_ddl: true,
            savepoints: true,
        }
    }

    fn server_version(&self) -> &ServerVersion {
        &self.version
    }

    async fn inspect(&mut self, scope: &Scope) -> Result<Database, Error> {
        let saved = self.enter_inspection().await?;
        let ctx = InspectContext {
            major: self.version.major,
            history_schema: self.history.schema.clone(),
        };
        let result = inspect::inspect(&self.client, &ctx, scope).await;
        let restored = self.leave_inspection(saved).await;
        let model = result?;
        restored?;
        Ok(model)
    }

    async fn execute(&mut self, sql: &str) -> Result<(), ExecError> {
        self.client
            .batch_execute(sql)
            .await
            .map_err(|e| exec_error(&e).with_statement(sql))
    }

    async fn begin(&mut self) -> Result<(), ExecError> {
        self.execute("BEGIN").await
    }

    async fn commit(&mut self) -> Result<(), ExecError> {
        self.execute("COMMIT").await
    }

    async fn rollback(&mut self) -> Result<(), ExecError> {
        self.execute("ROLLBACK").await
    }

    async fn lock(&mut self, wait: Duration) -> Result<(), Error> {
        let sql = format!(
            "SELECT pg_catalog.pg_try_advisory_lock({})",
            Self::lock_key_sql()
        );
        let deadline = Instant::now() + wait;
        loop {
            let acquired: bool = self
                .client
                .query_one(&sql, &[])
                .await
                .map_err(|e| exec_error(&e))?
                .get(0);
            if acquired {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::LockTimeout(format!(
                    "waited {:?} for another session to release the lock on database `{}`",
                    wait,
                    self.database()
                )));
            }
            tokio::time::sleep(LOCK_POLL.min(deadline - now)).await;
        }
    }

    async fn unlock(&mut self) -> Result<(), Error> {
        let sql = format!(
            "SELECT pg_catalog.pg_advisory_unlock({})",
            Self::lock_key_sql()
        );
        self.client
            .query_one(&sql, &[])
            .await
            .map_err(|e| exec_error(&e))?;
        Ok(())
    }

    async fn history_ensure(&mut self) -> Result<(), Error> {
        history::ensure(&self.client, &self.history).await
    }

    async fn history_load(&mut self) -> Result<Vec<HistoryRow>, Error> {
        history::load(&self.client, &self.history).await
    }

    async fn history_write(&mut self, row: &HistoryRow) -> Result<(), Error> {
        history::write(&self.client, &self.history, row).await
    }

    async fn history_delete(&mut self, version: &str) -> Result<(), Error> {
        history::delete(&self.client, &self.history, version).await
    }

    async fn reset_session(&mut self) -> Result<(), ExecError> {
        self.execute("RESET ALL").await?;
        self.apply_session_settings().await
    }

    async fn reset(&mut self) -> Result<(), Error> {
        if self.purpose != SessionPurpose::Scratch {
            return Err(Error::other(
                "refusing to reset a database that is not an OpenMig scratch database",
            ));
        }
        let work = self.database().to_owned();
        let (admin, _, _) = connect_checked(&self.spec.with_dbname(MAINTENANCE_DATABASE)).await?;
        admin_execute(
            &admin,
            &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", ident(&work)),
        )
        .await?;
        let roles: Vec<String> = admin
            .query(
                "SELECT rolname::text FROM pg_catalog.pg_roles \
                 WHERE oid <> 10 AND rolname !~ '^pg_' AND rolname <> current_user \
                 ORDER BY rolname COLLATE \"C\"",
                &[],
            )
            .await
            .map_err(|e| exec_error(&e))?
            .iter()
            .map(|r| r.get(0))
            .collect();
        if !roles.is_empty() {
            let list = roles
                .iter()
                .map(|r| ident(r))
                .collect::<Vec<_>>()
                .join(", ");
            admin_execute(&admin, &format!("DROP OWNED BY {list}")).await?;
            admin_execute(&admin, &format!("DROP ROLE {list}")).await?;
        }
        admin_execute(
            &admin,
            &format!("CREATE DATABASE {} TEMPLATE template0", ident(&work)),
        )
        .await?;
        drop(admin);
        let (client, version, current_user) = connect_checked(&self.spec).await?;
        self.client = client;
        self.version = version;
        self.current_user = current_user;
        self.apply_session_settings().await?;
        Ok(())
    }
}
