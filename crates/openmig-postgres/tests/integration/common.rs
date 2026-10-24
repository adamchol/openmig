use openmig_core::{Scope, Session, SessionPurpose};
use openmig_postgres::{Database, PgSession, PgSettings};
use tokio::sync::RwLock;

/// Roles are cluster-wide. Tests that create roles hold a read guard; resetting a scratch database
/// drops every role and holds the write guard.
pub static ROLES: RwLock<()> = RwLock::const_new(());

static SERVER: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

/// The server named by `OPENMIG_TEST_PG_URL`, or else one PostgreSQL server from the testkit pool
/// kept for the whole test process.
pub async fn admin_url() -> Option<String> {
    if let Ok(url) = std::env::var("OPENMIG_TEST_PG_URL")
        && !url.is_empty()
    {
        return Some(url);
    }
    let url = SERVER
        .get_or_init(|| async {
            let lease = Box::leak(Box::new(openmig_testkit::postgres().await));
            lease.url().to_owned()
        })
        .await;
    Some(url.clone())
}

/// The admin URL with another database.
pub fn url_for(admin: &str, database: &str) -> String {
    let mut url = url::Url::parse(admin).expect("OPENMIG_TEST_PG_URL is a URL");
    url.set_path(&format!("/{database}"));
    url.to_string()
}

pub async fn connect(url: &str, purpose: SessionPurpose) -> PgSession {
    PgSession::connect(url, &PgSettings::default(), purpose)
        .await
        .unwrap_or_else(|e| panic!("connect to {url}: {e}"))
}

pub async fn admin() -> PgSession {
    connect(
        &admin_url().await.expect("checked by the caller"),
        SessionPurpose::Target,
    )
    .await
}

pub async fn run(session: &mut PgSession, sql: &str) {
    if let Err(e) = session.execute(sql).await {
        panic!("{e}\n  position: {:?}\n{sql}", e.position);
    }
}

/// Replace the `__R__` role name prefix in a fixture.
pub fn with_prefix(sql: &str, prefix: &str) -> String {
    sql.replace("__R__", prefix)
}

/// Drop every role whose name starts with `prefix`.
pub async fn drop_roles(prefix: &str) {
    drop_roles_where("starts_with(rolname, $1)", prefix).await;
}

/// Drop the role `name` if it exists.
pub async fn drop_role(name: &str) {
    drop_roles_where("rolname = $1", name).await;
}

async fn drop_roles_where(condition: &str, value: &str) {
    let mut admin = admin().await;
    let roles: Vec<String> = admin
        .client()
        .query(
            &format!("SELECT rolname::text FROM pg_roles WHERE {condition} ORDER BY 1"),
            &[&value],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    for role in roles {
        run(
            &mut admin,
            &format!("DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\""),
        )
        .await;
    }
}

/// A database created for one test.
pub struct TestDb {
    pub name: String,
    pub url: String,
}

impl TestDb {
    /// Create an empty database (dropping a leftover of a previous run). `None` when no server
    /// is configured.
    pub async fn create(name: &str) -> Option<Self> {
        let admin_url = admin_url().await?;
        let mut admin = admin().await;
        run(
            &mut admin,
            &format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"),
        )
        .await;
        run(
            &mut admin,
            &format!("CREATE DATABASE \"{name}\" TEMPLATE template0"),
        )
        .await;
        Some(Self {
            name: name.to_owned(),
            url: url_for(&admin_url, name),
        })
    }

    pub async fn session(&self) -> PgSession {
        connect(&self.url, SessionPurpose::Target).await
    }

    pub async fn run(&self, sql: &str) {
        run(&mut self.session().await, sql).await;
    }

    pub async fn major(&self) -> u32 {
        self.session().await.server_version().major
    }

    pub async fn inspect(&self, scope: &Scope) -> Database {
        self.session()
            .await
            .inspect(scope)
            .await
            .unwrap_or_else(|e| panic!("inspect {}: {e}", self.name))
    }

    pub async fn drop(self) {
        let mut admin = admin().await;
        run(
            &mut admin,
            &format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name),
        )
        .await;
    }
}
