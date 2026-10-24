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
    #[error("cannot read {SUM_FILE_NAME}: {0}")]
    Io(String),
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

const HEADER_PREFIX: &str = "openmig-sum";
const FORMAT_VERSION: &str = "1";

impl SumFile {
    /// Parse and check the total. Lines may end in `\r\n`.
    pub fn parse(text: &str) -> Result<Self, SumError> {
        let malformed = |line: usize, message: &str| SumError::Malformed {
            line,
            message: message.to_owned(),
        };
        let mut lines = text.split_inclusive('\n').map(|l| {
            let l = l.strip_suffix('\n').unwrap_or(l);
            l.strip_suffix('\r').unwrap_or(l)
        });
        let header = lines.next().unwrap_or_default();
        let total = parse_header(header).map_err(|message| malformed(1, &message))?;

        let mut entries: Vec<(String, String)> = Vec::new();
        let mut body = String::new();
        for (index, line) in lines.enumerate() {
            let line_no = index + 2;
            let Some((name, hash)) = line.split_once(' ') else {
                return Err(malformed(line_no, "expected `<file name> <sha256 hex>`"));
            };
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(malformed(line_no, "expected `<file name> <sha256 hex>`"));
            }
            if !is_sha256_hex(hash) {
                return Err(malformed(
                    line_no,
                    "the checksum is not a lowercase sha256 hex digest",
                ));
            }
            if let Some((previous, _)) = entries.last() {
                if previous.as_str() == name {
                    return Err(malformed(line_no, &format!("duplicate entry for `{name}`")));
                }
                if previous.as_str() > name {
                    return Err(malformed(line_no, "entries are not sorted by file name"));
                }
            }
            body.push_str(line);
            body.push('\n');
            entries.push((name.to_owned(), hash.to_owned()));
        }
        if sha256_hex(body.as_bytes()) != total {
            return Err(SumError::TotalMismatch);
        }
        Ok(Self { entries })
    }

    pub fn render(&self) -> String {
        let body: String = self
            .entries
            .iter()
            .map(|(name, hash)| format!("{name} {hash}\n"))
            .collect();
        let total = sha256_hex(body.as_bytes());
        format!("{HEADER_PREFIX} {FORMAT_VERSION} total:{total}\n{body}")
    }

    /// Read `dir/openmig.sum`; `None` when it does not exist. A malformed file is an
    /// [`std::io::ErrorKind::InvalidData`] error wrapping the [`SumError`].
    pub fn read(dir: &Path) -> Result<Option<Self>, std::io::Error> {
        let Some(text) = read_text(dir)? else {
            return Ok(None);
        };
        Self::parse(&text)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

pub(crate) fn read_text(dir: &Path) -> Result<Option<String>, std::io::Error> {
    match std::fs::read_to_string(dir.join(SUM_FILE_NAME)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_header(header: &str) -> Result<String, String> {
    let expected = format!("expected `{HEADER_PREFIX} {FORMAT_VERSION} total:<sha256 hex>`");
    let mut fields = header.split(' ');
    if fields.next() != Some(HEADER_PREFIX) {
        return Err(expected);
    }
    match fields.next() {
        Some(FORMAT_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "unsupported format version `{other}` (this OpenMig reads version {FORMAT_VERSION})"
            ));
        }
        None => return Err(expected),
    }
    match (
        fields.next().and_then(|f| f.strip_prefix("total:")),
        fields.next(),
    ) {
        (Some(total), None) if is_sha256_hex(total) => Ok(total.to_owned()),
        _ => Err(expected),
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
