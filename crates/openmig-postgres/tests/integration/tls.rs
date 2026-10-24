//! TLS modes against a server with TLS enabled, named by `OPENMIG_TEST_PG_TLS_URL` (TCP, host
//! `localhost`, no `sslmode`) whose certificate is signed by the CA in `OPENMIG_TEST_PG_TLS_CA`
//! and valid for `localhost` but not for `127.0.0.2` (where the server must also listen).

use openmig_core::{Error, SessionPurpose};
use openmig_postgres::{PgSession, PgSettings};

async fn connect(url: &str) -> Result<PgSession, Error> {
    PgSession::connect(url, &PgSettings::default(), SessionPurpose::Target).await
}

async fn encrypted(url: &str) -> bool {
    let session = connect(url).await.unwrap_or_else(|e| panic!("{url}: {e}"));
    session
        .client()
        .query_one(
            "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn tls_modes() {
    let (Ok(base), Ok(ca)) = (
        std::env::var("OPENMIG_TEST_PG_TLS_URL"),
        std::env::var("OPENMIG_TEST_PG_TLS_CA"),
    ) else {
        eprintln!("skipping: set OPENMIG_TEST_PG_TLS_URL and OPENMIG_TEST_PG_TLS_CA");
        return;
    };
    let with = |url: &str, query: &str| format!("{url}?{query}");
    let mut other_host = url::Url::parse(&base).unwrap();
    other_host.set_host(Some("127.0.0.2")).unwrap();
    let other_host = other_host.to_string();

    assert!(!encrypted(&with(&base, "sslmode=disable")).await);
    assert!(encrypted(&base).await, "prefer uses TLS when offered");
    assert!(encrypted(&with(&base, "sslmode=require")).await);
    assert!(encrypted(&with(&other_host, "sslmode=require")).await);
    let verify_full = format!("sslmode=verify-full&sslrootcert={ca}");
    let verify_ca = format!("sslmode=verify-ca&sslrootcert={ca}");
    assert!(encrypted(&with(&base, &verify_full)).await);
    assert!(encrypted(&with(&base, &verify_ca)).await);
    assert!(encrypted(&with(&other_host, &verify_ca)).await);

    let err = connect(&with(&other_host, &verify_full)).await.err();
    assert!(
        matches!(&err, Some(Error::Connect { message, .. }) if message.contains("certificate")),
        "{err:?}"
    );
    let err = connect(&with(&base, "sslmode=verify-full")).await.err();
    assert!(
        matches!(err, Some(Error::Connect { .. })),
        "untrusted CA: {err:?}"
    );
}
