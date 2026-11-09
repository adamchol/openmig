use crate::common::admin;
use openmig_core::Session;

/// `openmig_postgres::casts` lists exactly the server's casts between `pg_catalog` types.
#[tokio::test]
async fn builtin_casts_match_the_server() {
    let s = admin().await;
    let major = s.server_version().major;
    let rows = s
        .client()
        .query(
            "SELECT format_type(c.castsource, NULL), format_type(c.casttarget, NULL) \
             FROM pg_cast c \
             JOIN pg_type s ON s.oid = c.castsource \
             JOIN pg_type t ON t.oid = c.casttarget \
             WHERE s.typnamespace = 'pg_catalog'::regnamespace \
               AND t.typnamespace = 'pg_catalog'::regnamespace",
            &[],
        )
        .await
        .unwrap();
    let mut server: Vec<(String, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    server.sort();
    let known: Vec<(String, String)> = openmig_postgres::casts::builtin_casts(major)
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
    let missing: Vec<_> = server.iter().filter(|c| !known.contains(c)).collect();
    let extra: Vec<_> = known.iter().filter(|c| !server.contains(c)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "PostgreSQL {major}: casts missing from the table: {missing:?}; not on the server: {extra:?}"
    );
}
