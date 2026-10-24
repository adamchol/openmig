//! Integration tests against a PostgreSQL server named by `OPENMIG_TEST_PG_URL` (an
//! administrative URL, e.g. `postgresql:///postgres?host=/tmp/pg&port=5432&user=openmig`).
//! Without it every test prints a notice and passes.

mod common;
mod dialo;
mod inspect;
mod session;
