use crate::directives::{Directives, GENERATED, NO_TRANSACTION, parse_directives};
use crate::sum::{self, SUM_FILE_NAME, SumError, SumFile, sha256_hex};
use openmig_core::{Error, Plan};
use std::collections::BTreeMap;
use std::io::Write;
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

impl MigrationFile {
    fn new(path: PathBuf, file_name: &str, content: String) -> Result<Self, Error> {
        let (version, name) = parse_file_name(file_name)
            .map_err(|e| Error::Migrations(format!("{}: {}", path.display(), e)))?;
        let directives = parse_directives(&content)
            .map_err(|e| Error::Migrations(format!("{}: {e}", path.display())))?;
        Ok(Self {
            version,
            name,
            file_name: file_name.to_owned(),
            checksum: sha256_hex(content.as_bytes()),
            path,
            content,
            directives,
        })
    }
}

/// A loaded migration directory, files sorted by version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationDir {
    pub path: PathBuf,
    pub files: Vec<MigrationFile>,
}

const VERSION_FORMAT: &str = "%Y%m%d%H%M%S";

impl MigrationDir {
    /// Load every `*.sql` file. A missing directory loads as empty. Errors on invalid names,
    /// duplicate versions and invalid directives.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    files: Vec::new(),
                });
            }
            Err(e) => {
                return Err(Error::io(
                    format!("cannot read migration directory {}", path.display()),
                    e,
                ));
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                Error::io(
                    format!("cannot read migration directory {}", path.display()),
                    e,
                )
            })?;
            let file_path = entry.path();
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                if file_name.to_string_lossy().ends_with(".sql") {
                    return Err(Error::Migrations(format!(
                        "{}: file name is not valid UTF-8",
                        file_path.display()
                    )));
                }
                continue;
            };
            if !file_name.ends_with(".sql") || !file_path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&file_path)
                .map_err(|e| Error::io(format!("cannot read {}", file_path.display()), e))?;
            let content = String::from_utf8(bytes).map_err(|_| {
                Error::Migrations(format!("{}: not valid UTF-8", file_path.display()))
            })?;
            files.push(MigrationFile::new(file_path, file_name, content)?);
        }
        files.sort_by(|a, b| (&a.version, &a.file_name).cmp(&(&b.version, &b.file_name)));
        if let Some(pair) = files.windows(2).find(|w| w[0].version == w[1].version) {
            return Err(Error::Migrations(format!(
                "{}: duplicate migration version {}: {} and {}",
                path.display(),
                pair[0].version,
                pair[0].file_name,
                pair[1].file_name
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            files,
        })
    }

    /// Compare files with `openmig.sum`. An empty directory without a sum file is valid.
    pub fn verify_sum(&self) -> Result<(), SumError> {
        let text = sum::read_text(&self.path).map_err(|e| SumError::Io(e.to_string()))?;
        let Some(text) = text else {
            return if self.files.is_empty() {
                Ok(())
            } else {
                Err(SumError::Missing)
            };
        };
        let sum = SumFile::parse(&text)?;
        let recorded: BTreeMap<&str, &str> = sum
            .entries
            .iter()
            .map(|(name, hash)| (name.as_str(), hash.as_str()))
            .collect();
        let actual: BTreeMap<&str, &str> = self
            .files
            .iter()
            .map(|f| (f.file_name.as_str(), f.checksum.as_str()))
            .collect();
        let added: Vec<String> = actual
            .keys()
            .filter(|name| !recorded.contains_key(*name))
            .map(|name| name.to_string())
            .collect();
        let removed: Vec<String> = recorded
            .keys()
            .filter(|name| !actual.contains_key(*name))
            .map(|name| name.to_string())
            .collect();
        let edited: Vec<String> = actual
            .iter()
            .filter(|(name, hash)| recorded.get(*name).is_some_and(|h| h != *hash))
            .map(|(name, _)| name.to_string())
            .collect();
        if added.is_empty() && removed.is_empty() && edited.is_empty() {
            Ok(())
        } else {
            Err(SumError::Mismatch {
                added,
                removed,
                edited,
            })
        }
    }

    /// The sum file for the current files.
    pub fn sum(&self) -> SumFile {
        let mut entries: Vec<(String, String)> = self
            .files
            .iter()
            .map(|f| (f.file_name.clone(), f.checksum.clone()))
            .collect();
        entries.sort();
        SumFile { entries }
    }

    /// Rewrite `openmig.sum` from the files, creating the directory if needed.
    pub fn write_sum(&self) -> Result<(), Error> {
        std::fs::create_dir_all(&self.path)
            .map_err(|e| Error::io(format!("cannot create {}", self.path.display()), e))?;
        let path = self.path.join(SUM_FILE_NAME);
        std::fs::write(&path, self.sum().render())
            .map_err(|e| Error::io(format!("cannot write {}", path.display()), e))
    }

    /// The version for a new file created at `now`: its UTC timestamp, bumped by one second past
    /// the newest existing version when the clock is behind.
    pub fn next_version(&self, now: jiff::Timestamp) -> String {
        let candidate = now
            .to_zoned(jiff::tz::TimeZone::UTC)
            .strftime(VERSION_FORMAT)
            .to_string();
        match self.latest_version() {
            Some(latest) if candidate.as_str() <= latest => bump_version(latest),
            _ => candidate,
        }
    }

    /// Write a new migration file and update `openmig.sum`.
    pub fn add(
        &mut self,
        version: &str,
        name: &str,
        content: &str,
    ) -> Result<&MigrationFile, Error> {
        let file_name = file_name(version, name);
        let path = self.path.join(&file_name);
        let file = MigrationFile::new(path.clone(), &file_name, content.to_owned())?;
        if let Some(existing) = self.files.iter().find(|f| f.version == file.version) {
            return Err(Error::Migrations(format!(
                "{}: version {} is already used by {}",
                path.display(),
                file.version,
                existing.file_name
            )));
        }
        std::fs::create_dir_all(&self.path)
            .map_err(|e| Error::io(format!("cannot create {}", self.path.display()), e))?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
            .map_err(|e| Error::io(format!("cannot write {}", path.display()), e))?;
        let index = self.files.partition_point(|f| f.version < file.version);
        self.files.insert(index, file);
        self.write_sum()?;
        Ok(&self.files[index])
    }

    /// Delete a file and update `openmig.sum`.
    pub fn remove(&mut self, file_name: &str) -> Result<(), Error> {
        let index = self
            .files
            .iter()
            .position(|f| f.file_name == file_name)
            .ok_or_else(|| {
                Error::Migrations(format!(
                    "{}: no migration file named {file_name}",
                    self.path.display()
                ))
            })?;
        let path = &self.files[index].path;
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(format!("cannot delete {}", path.display()), e)),
        }
        self.files.remove(index);
        self.write_sum()
    }

    pub fn latest_version(&self) -> Option<&str> {
        self.files.last().map(|f| f.version.as_str())
    }
}

/// One second after `version`. Versions that are not valid timestamps are incremented as numbers.
fn bump_version(version: &str) -> String {
    match jiff::civil::DateTime::strptime(VERSION_FORMAT, version) {
        Ok(datetime) => match datetime.checked_add(jiff::SignedDuration::from_secs(1)) {
            Ok(next) => next.strftime(VERSION_FORMAT).to_string(),
            Err(_) => increment_number(version),
        },
        Err(_) => increment_number(version),
    }
}

fn increment_number(version: &str) -> String {
    let next = version.parse::<u64>().map_or(0, |v| v.saturating_add(1));
    format!("{next:014}")
}

/// `<version>[_<name>].sql`. `name` must match `[a-z0-9_]+`.
pub fn file_name(version: &str, name: &str) -> String {
    if name.is_empty() {
        format!("{version}.sql")
    } else {
        format!("{version}_{name}.sql")
    }
}

/// Split a file name into `(version, name)`. Valid names match `^\d{14}(_[a-z0-9_]+)?\.sql$`.
pub fn parse_file_name(file: &str) -> Result<(String, String), NameError> {
    let error = |reason: &str| NameError {
        file: file.to_owned(),
        reason: reason.to_owned(),
    };
    let stem = file
        .strip_suffix(".sql")
        .ok_or_else(|| error("migration files must end in `.sql`"))?;
    let digits = stem.bytes().take_while(u8::is_ascii_digit).count();
    if digits < 14 {
        return Err(error(
            "expected `<version>[_<name>].sql` where the version is a 14-digit UTC timestamp (YYYYMMDDHHMMSS)",
        ));
    }
    let (version, rest) = stem.split_at(14);
    if rest.is_empty() {
        return Ok((version.to_owned(), String::new()));
    }
    let name = rest
        .strip_prefix('_')
        .ok_or_else(|| error("the 14-digit version must be followed by `_<name>` or `.sql`"))?;
    if name.is_empty() {
        return Err(error("the name after `_` is empty"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(error(
            "the name may only contain lowercase letters, digits and `_`",
        ));
    }
    Ok((version.to_owned(), name.to_owned()))
}

/// The content of a migration file for `plan`: directives (`generated`, plus `no-transaction`
/// when required), a blank line, then every statement preceded by its `-- comment`. Line breaks in
/// comments are replaced by spaces so every comment stays one line.
pub fn render_file(plan: &Plan, generated: bool) -> String {
    let mut out = String::new();
    if generated {
        out.push_str(&format!("-- openmig:{GENERATED}\n"));
    }
    if plan.requires_no_transaction() {
        out.push_str(&format!("-- openmig:{NO_TRANSACTION}\n"));
    }
    if !out.is_empty() {
        out.push('\n');
    }
    for statement in &plan.statements {
        if !statement.comment.is_empty() {
            out.push_str("-- ");
            out.push_str(&statement.comment.replace(['\r', '\n'], " "));
            out.push('\n');
        }
        out.push_str(&statement.sql);
        out.push_str(";\n");
    }
    out
}

#[cfg(test)]
mod tests;
