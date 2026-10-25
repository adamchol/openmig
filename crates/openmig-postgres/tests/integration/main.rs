//! Integration tests against a PostgreSQL server: the one named by `OPENMIG_TEST_PG_URL` (an
//! administrative URL, e.g. `postgresql:///postgres?host=/tmp/pg&port=5432&user=openmig`), or else
//! a scratch server from the testkit pool.

mod common;
mod corpus;
mod corpus_replay;
mod inspect;
mod plan;
mod roundtrip;
mod session;
mod tls;
