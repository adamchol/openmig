use crate::directives::Directives;
use crate::sum::SumError;
use openmig_core::Plan;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid migration file name `{file}`: {reason}")]
pub struct NameError {
    pub file: String,
    pub reason: String,
}

/// One migration file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationFile {
    /// `20261024120000`.
    pub version: String,
    /// `add_users`, or empty.
    pub name: String,
    /// `20261024120000_add_users.sql`.
    pub file_name: String,
    pub path: PathBuf,
    pub content: String,
    /// sha256 hex of `content`.
    pub checksum: String,
    pub directives: Directives,
}

/// A loaded migration directory, files sorted by version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationDir {
    pub path: PathBuf,
    pub files: Vec<MigrationFile>,
}

impl MigrationDir {
    /// Load every `*.sql` file. A missing directory loads as empty. Errors on invalid names,
    /// duplicate versions and invalid directives.
    pub fn load(path: &Path) -> Result<Self, openmig_core::Error> {
        let _ = path;
        unimplemented!("migration dir load")
    }

    /// Compare files with `openmig.sum`. An empty directory without a sum file is valid.
    pub fn verify_sum(&self) -> Result<(), SumError> {
        unimplemented!("verify sum")
    }

    /// Rewrite `openmig.sum` from the files.
    pub fn write_sum(&self) -> Result<(), openmig_core::Error> {
        unimplemented!("write sum")
    }

    /// The version for a new file created at `now`: its UTC timestamp, bumped by one second past
    /// the newest existing version when the clock is behind.
    pub fn next_version(&self, now: jiff::Timestamp) -> String {
        let _ = now;
        unimplemented!("next version")
    }

    /// Write a new migration file and update `openmig.sum`.
    pub fn add(
        &mut self,
        version: &str,
        name: &str,
        content: &str,
    ) -> Result<&MigrationFile, openmig_core::Error> {
        let _ = (version, name, content);
        unimplemented!("add migration")
    }

    /// Delete a file and update `openmig.sum`.
    pub fn remove(&mut self, file_name: &str) -> Result<(), openmig_core::Error> {
        let _ = file_name;
        unimplemented!("remove migration")
    }

    pub fn latest_version(&self) -> Option<&str> {
        self.files.last().map(|f| f.version.as_str())
    }
}

/// `<version>[_<name>].sql`. `name` must match `[a-z0-9_]+`.
pub fn file_name(version: &str, name: &str) -> String {
    if name.is_empty() {
        format!("{version}.sql")
    } else {
        format!("{version}_{name}.sql")
    }
}

/// Split a file name into `(version, name)`.
pub fn parse_file_name(file: &str) -> Result<(String, String), NameError> {
    let _ = file;
    unimplemented!("parse file name")
}

/// The content of a migration file for `plan`: directives (`generated`, plus `no-transaction`
/// when required), a blank line, then every statement preceded by its `-- comment`.
pub fn render_file(plan: &Plan, generated: bool) -> String {
    let _ = (plan, generated);
    unimplemented!("render file")
}
