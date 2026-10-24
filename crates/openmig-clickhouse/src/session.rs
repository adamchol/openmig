use crate::http::HttpClient;
use crate::privileges::{PrivilegeCatalog, PrivilegeRow};
use crate::settings::{ChSettings, SYSTEM_DATABASES};
use crate::{ClickHouseEngine, history, inspect};
use openmig_core::sql::{Dialect, quote_ident, quote_literal};
use openmig_core::{
    Capabilities, Error, ExecError, ExecErrorKind, HistoryRow, Scope, ServerVersion, Session,
    SessionPurpose,
};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Oldest server version OpenMig supports.
pub const MIN_SERVER_VERSION: (u32, u32) = (24, 8);

/// Settings for statements run by [`Session::execute`]: errors are reported only after the
/// statement finished everywhere, and mutations, `ALTER`s and `ON CLUSTER` DDL wait for every
/// replica.
const DDL_SETTINGS: &[(&str, &str)] = &[
    ("wait_end_of_query", "1"),
    ("alter_sync", "2"),
    ("mutations_sync", "2"),
    ("distributed_ddl_output_mode", "throw"),
    ("distributed_ddl_task_timeout", "300"),
];

/// A connection to a ClickHouse server over HTTP.
///
/// HTTP is stateless; statements run by [`Session::execute`] share a server-side HTTP session
/// (`session_id`), so `SET` statements in a migration file last until
/// [`Session::reset_session`] starts a new one. Internal queries (inspection, history, lock) run
/// outside that session with fixed settings.
pub struct ChSession {
    pub(crate) http: HttpClient,
    pub(crate) settings: ChSettings,
    purpose: SessionPurpose,
    version: ServerVersion,
    pub(crate) current_user: String,
    session_id: String,
    privileges: Option<PrivilegeCatalog>,
    pub(crate) lock: history::LockState,
    pub(crate) history: history::HistoryState,
}

#[derive(Deserialize)]
struct Hello {
    version: String,
    user: String,
}

impl ChSession {
    pub async fn connect(
        url: &str,
        settings: &ChSettings,
        purpose: SessionPurpose,
    ) -> Result<Self, Error> {
        let http = HttpClient::new(url)?;
        let hello: Vec<Hello> = http
            .rows("SELECT version() AS version, currentUser() AS user", &[])
            .await
            .map_err(|e| Error::Connect {
                target: http.target().to_owned(),
                message: e.to_string(),
            })?;
        let hello = hello.into_iter().next().ok_or_else(|| Error::Connect {
            target: http.target().to_owned(),
            message: "the server returned no version".into(),
        })?;
        let version = parse_version(&hello.version).ok_or_else(|| Error::Connect {
            target: http.target().to_owned(),
            message: format!("cannot parse server version `{}`", hello.version),
        })?;
        if (version.major, version.minor) < MIN_SERVER_VERSION {
            return Err(Error::Connect {
                target: http.target().to_owned(),
                message: format!(
                    "ClickHouse {} is not supported; OpenMig needs {}.{} or later",
                    version.text, MIN_SERVER_VERSION.0, MIN_SERVER_VERSION.1
                ),
            });
        }
        Ok(Self {
            http,
            settings: settings.clone(),
            purpose,
            version,
            current_user: hello.user,
            session_id: new_session_id(),
            privileges: None,
            lock: history::LockState::default(),
            history: history::HistoryState::default(),
        })
    }

    pub fn purpose(&self) -> SessionPurpose {
        self.purpose
    }

    /// The user this session is authenticated as (`currentUser()`).
    pub fn current_user(&self) -> &str {
        &self.current_user
    }

    /// The server's privilege hierarchy, read once per session.
    pub async fn privileges(&mut self) -> Result<&PrivilegeCatalog, Error> {
        if self.privileges.is_none() {
            let rows: Vec<PrivilegeRow> = self
                .http
                .rows(
                    "SELECT privilege, aliases, level, parent_group FROM system.privileges",
                    &[],
                )
                .await
                .map_err(|e| Error::Inspect(format!("cannot read system.privileges: {e}")))?;
            self.privileges = Some(PrivilegeCatalog::new(rows));
        }
        Ok(self.privileges.get_or_insert_default())
    }

    /// Run an internal statement outside the `execute` session.
    pub(crate) async fn run(&self, sql: &str, settings: &[(&str, &str)]) -> Result<(), ExecError> {
        let mut all = DDL_SETTINGS.to_vec();
        all.extend_from_slice(settings);
        self.http
            .query(sql, &all)
            .await
            .map(drop)
            .map_err(|e| e.with_statement(sql))
    }

    async fn names(&self, sql: &str) -> Result<Vec<String>, Error> {
        #[derive(Deserialize)]
        struct Name {
            name: String,
        }
        let rows: Vec<Name> = self.http.rows(sql, &[]).await.map_err(Error::Exec)?;
        Ok(rows.into_iter().map(|r| r.name).collect())
    }
}

impl Session<ClickHouseEngine> for ChSession {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactional_ddl: false,
            savepoints: false,
        }
    }

    fn server_version(&self) -> &ServerVersion {
        &self.version
    }

    async fn inspect(&mut self, scope: &Scope) -> Result<crate::model::Model, Error> {
        inspect::inspect(self, scope).await
    }

    async fn execute(&mut self, sql: &str) -> Result<(), ExecError> {
        let mut settings = DDL_SETTINGS.to_vec();
        settings.push(("session_id", &self.session_id));
        self.http
            .query(sql, &settings)
            .await
            .map(drop)
            .map_err(|e| e.with_statement(sql))
    }

    async fn begin(&mut self) -> Result<(), ExecError> {
        Err(no_transactions())
    }

    async fn commit(&mut self) -> Result<(), ExecError> {
        Err(no_transactions())
    }

    async fn rollback(&mut self) -> Result<(), ExecError> {
        Err(no_transactions())
    }

    async fn lock(&mut self, wait: Duration) -> Result<(), Error> {
        history::lock(self, wait).await
    }

    async fn unlock(&mut self) -> Result<(), Error> {
        history::unlock(self).await
    }

    async fn history_ensure(&mut self) -> Result<(), Error> {
        history::ensure(self).await
    }

    async fn history_load(&mut self) -> Result<Vec<HistoryRow>, Error> {
        history::load(self).await
    }

    async fn history_write(&mut self, row: &HistoryRow) -> Result<(), Error> {
        history::write(self, row).await
    }

    async fn history_delete(&mut self, version: &str) -> Result<(), Error> {
        history::delete(self, version).await
    }

    async fn reset_session(&mut self) -> Result<(), ExecError> {
        self.session_id = new_session_id();
        Ok(())
    }

    /// Drops every non-system database (`SYNC`, re-creating `default`), SQL-defined row policies,
    /// quotas, settings profiles, users (except the connecting one), roles, functions and named
    /// collections.
    async fn reset(&mut self) -> Result<(), Error> {
        if self.purpose != SessionPurpose::Scratch {
            return Err(Error::other(
                "refusing to reset a database that is not an OpenMig scratch server",
            ));
        }
        let q = |s: &str| quote_ident(s, Dialect::ClickHouse);
        let system = SYSTEM_DATABASES
            .iter()
            .map(|d| quote_literal(d, Dialect::ClickHouse))
            .collect::<Vec<_>>()
            .join(", ");
        for db in self
            .names(&format!(
                "SELECT name FROM system.databases WHERE name NOT IN ({system}) ORDER BY name"
            ))
            .await?
        {
            self.run(&format!("DROP DATABASE IF EXISTS {} SYNC", q(&db)), &[])
                .await?;
        }
        self.run("CREATE DATABASE IF NOT EXISTS `default`", &[])
            .await?;

        #[derive(Deserialize)]
        struct Policy {
            short_name: String,
            database: String,
            table: String,
        }
        let policies: Vec<Policy> = self
            .http
            .rows(
                "SELECT short_name, database, table FROM system.row_policies \
                 WHERE storage != 'users_xml' ORDER BY short_name, database, table",
                &[],
            )
            .await?;
        for p in policies {
            self.run(
                &format!(
                    "DROP ROW POLICY IF EXISTS {} ON {}.{}",
                    q(&p.short_name),
                    q(&p.database),
                    q(&p.table)
                ),
                &[],
            )
            .await?;
        }
        let current = quote_literal(&self.current_user, Dialect::ClickHouse);
        for (table, kind, filter) in [
            ("quotas", "QUOTA", String::new()),
            ("settings_profiles", "SETTINGS PROFILE", String::new()),
            ("users", "USER", format!(" AND name != {current}")),
            ("roles", "ROLE", String::new()),
        ] {
            for name in self
                .names(&format!(
                    "SELECT name FROM system.{table} WHERE storage != 'users_xml'{filter} ORDER BY name"
                ))
                .await?
            {
                self.run(&format!("DROP {kind} IF EXISTS {}", q(&name)), &[])
                    .await?;
            }
        }
        for name in self
            .names(
                "SELECT name FROM system.functions WHERE origin = 'SQLUserDefined' ORDER BY name",
            )
            .await?
        {
            self.run(&format!("DROP FUNCTION IF EXISTS {}", q(&name)), &[])
                .await?;
        }
        for name in self
            .names("SELECT name FROM system.named_collections ORDER BY name")
            .await?
        {
            self.run(
                &format!("DROP NAMED COLLECTION IF EXISTS {}", q(&name)),
                &[],
            )
            .await?;
        }
        self.session_id = new_session_id();
        self.lock = history::LockState::default();
        self.history = history::HistoryState::default();
        Ok(())
    }
}

fn no_transactions() -> ExecError {
    ExecError {
        kind: ExecErrorKind::Other,
        message: "ClickHouse has no transactions".into(),
        ..ExecError::default()
    }
}

fn parse_version(text: &str) -> Option<ServerVersion> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some(ServerVersion {
        major,
        minor,
        text: text.to_owned(),
    })
}

/// A server-side HTTP session id unique to this process and call.
fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "openmig-{}-{nanos:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_session_ids() {
        let v = parse_version("26.5.1.882").unwrap();
        assert_eq!((v.major, v.minor), (26, 5));
        assert_eq!(v.text, "26.5.1.882");
        assert!(parse_version("garbage").is_none());
        assert_ne!(new_session_id(), new_session_id());
    }
}
