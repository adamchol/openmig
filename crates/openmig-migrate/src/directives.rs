//! `-- openmig:<directive>` lines in the leading comment block of a migration file.
//!
//! The leading comment block is the run of `--` comment lines (and blank lines) at the top of the
//! file before the first statement. Recognised directives:
//!
//! * `-- openmig:generated` — written by `openmig generate`; `generate --base` may replace the file.
//! * `-- openmig:no-transaction` — run statement by statement outside a transaction.
//! * `-- openmig:lock-timeout <duration>` — lock timeout for this file (`30s`, `2m`).
//! * `-- openmig:statement-timeout <duration>`.
//!
//! Unknown `openmig:` directives are errors.

use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directives {
    pub generated: bool,
    pub no_transaction: bool,
    pub lock_timeout: Option<Duration>,
    pub statement_timeout: Option<Duration>,
}

/// Parse directives from the leading comment block of `content`.
pub fn parse_directives(content: &str) -> Result<Directives, String> {
    let _ = content;
    unimplemented!("directives")
}
