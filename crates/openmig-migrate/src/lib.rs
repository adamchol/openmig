//! Versioned migrations: migration directories, checksum files, directives and the deploy executor.
//!
//! A migration directory holds `<version>[_<name>].sql` files (version = 14-digit UTC timestamp
//! `YYYYMMDDHHMMSS`) and an `openmig.sum` checksum file. Other files are ignored.

mod dir;
mod directives;
mod executor;
mod sum;

pub use dir::{MigrationDir, MigrationFile, NameError, file_name, parse_file_name, render_file};
pub use directives::{Directives, parse_directives};
pub use executor::{
    DeployOptions, DeployReport, FileOutcome, Progress, Status, StatusEntry, StatusState, deploy,
    replay, status,
};
pub use sum::{SUM_FILE_NAME, SumError, SumFile, sha256_hex};
