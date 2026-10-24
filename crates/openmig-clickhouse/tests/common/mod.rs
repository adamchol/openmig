//! Helpers shared by the integration tests.

#![allow(dead_code)]

use openmig_clickhouse::{ChSession, ChSettings, ClickHouseEngine, Model};
use openmig_core::sql::split;
use openmig_core::{Dialect, Engine, Session, SessionPurpose};
use openmig_testkit::ChLease;

pub async fn connect(lease: &ChLease, settings: &ChSettings) -> ChSession {
    ClickHouseEngine::connect(lease.url(), settings, SessionPurpose::Scratch)
        .await
        .expect("connect to the scratch server")
}

/// Run every statement of a script, panicking with the failing statement.
pub async fn run(session: &mut ChSession, script: &str) {
    for statement in split(script, Dialect::ClickHouse).expect("split script") {
        if let Err(error) = session.execute(&statement.sql).await {
            panic!("{}\n{error}", statement.sql);
        }
    }
}

/// The model's `Debug` output without empty fields (`None`, `[]`, `{}`, `false`), for snapshots.
pub fn compact(model: &Model) -> String {
    format!("{model:#?}")
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.ends_with(": None,")
                || trimmed.ends_with(": [],")
                || trimmed.ends_with(": {},")
                || trimmed.ends_with(": false,"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
