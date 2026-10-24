//! `openmig.sum`: checksums of every migration file.
//!
//! ```text
//! openmig-sum 1 total:<sha256 hex of every following line, each including its "\n">
//! 20260216163954_baseline.sql <sha256 hex of the file bytes>
//! 20260217163957.sql <sha256 hex>
//! ```
//!
//! Lines are sorted by file name. Because the first line changes whenever any file is added or
//! edited, two branches that both add migrations conflict in git.

use std::path::Path;

pub const SUM_FILE_NAME: &str = "openmig.sum";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SumError {
    #[error("{SUM_FILE_NAME} is missing; run `openmig rehash`")]
    Missing,
    #[error("{SUM_FILE_NAME} is malformed at line {line}: {message}")]
    Malformed { line: usize, message: String },
    #[error(
        "{SUM_FILE_NAME} total does not match its lines (edited by hand?); run `openmig rehash` after reviewing"
    )]
    TotalMismatch,
    #[error(
        "migration files do not match {SUM_FILE_NAME}:{}",
        format_changes(added, removed, edited)
    )]
    Mismatch {
        added: Vec<String>,
        removed: Vec<String>,
        edited: Vec<String>,
    },
}

fn format_changes(added: &[String], removed: &[String], edited: &[String]) -> String {
    let mut s = String::new();
    for (label, files) in [("added", added), ("removed", removed), ("edited", edited)] {
        for f in files {
            s.push_str(&format!("\n  {label}: {f}"));
        }
    }
    s
}

/// Parsed `openmig.sum`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SumFile {
    /// `(file name, sha256 hex)` sorted by file name.
    pub entries: Vec<(String, String)>,
}

impl SumFile {
    pub fn parse(text: &str) -> Result<Self, SumError> {
        let _ = text;
        unimplemented!("sum parse")
    }

    pub fn render(&self) -> String {
        unimplemented!("sum render")
    }

    pub fn read(dir: &Path) -> Result<Option<Self>, std::io::Error> {
        let _ = dir;
        unimplemented!("sum read")
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
