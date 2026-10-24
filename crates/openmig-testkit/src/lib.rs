//! Test support: shared scratch servers and golden convergence cases.

mod servers;

pub use servers::{ChLease, Lease, PgLease, clickhouse, clickhouse_spec, postgres, postgres_spec};
