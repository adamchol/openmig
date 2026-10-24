use crate::object::ObjectRef;
use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Classification of a failed statement, used for retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecErrorKind {
    /// Waiting for a lock timed out (PostgreSQL `55P03`). Deploy retries transactional files.
    LockTimeout,
    /// The statement references an object that does not exist yet (PostgreSQL `42P01`, `42883`,
    /// `42704`, `3F000`; ClickHouse `UNKNOWN_TABLE`, `UNKNOWN_DATABASE`, ...). Scratch apply moves
    /// such statements to the end of the queue and retries them.
    UndefinedObject,
    /// The connection was lost.
    Connection,
    #[default]
    Other,
}

/// A statement failed on the server.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub message: String,
    /// SQLSTATE (PostgreSQL) or error code name (ClickHouse).
    pub code: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// 1-based character position inside the statement, when the server reports one.
    pub position: Option<usize>,
    /// The statement that failed.
    pub statement: Option<String>,
}

impl ExecError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    pub fn with_statement(mut self, statement: impl Into<String>) -> Self {
        self.statement = Some(statement.into());
        self
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(code) = &self.code {
            write!(f, " ({code})")?;
        }
        if let Some(detail) = &self.detail {
            write!(f, "\n  detail: {detail}")?;
        }
        if let Some(hint) = &self.hint {
            write!(f, "\n  hint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ExecError {}

/// Errors shared by every OpenMig crate. Each variant maps to a process exit code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    /// The schema could not be loaded or decoded; diagnostics are rendered already.
    #[error("{rendered}")]
    Schema { rendered: String },
    #[error("cannot connect to {target}: {message}")]
    Connect { target: String, message: String },
    #[error("{0}")]
    Exec(#[from] ExecError),
    #[error("cannot inspect the database: {0}")]
    Inspect(String),
    #[error("unsupported change to {object}: {reason}")]
    Unsupported {
        object: ObjectRef,
        reason: String,
        hint: Option<String>,
    },
    #[error("refusing to apply destructive changes without confirmation")]
    DestructiveRefused,
    #[error("timed out waiting for the migration lock ({0})")]
    LockTimeout(String),
    #[error("{0}")]
    Migrations(String),
    /// Drift between two states that were expected to be equal (`check`, `status --drift`).
    #[error("{0}")]
    Drift(String),
    #[error(transparent)]
    Scratch(#[from] openmig_scratch::ScratchError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }

    /// Process exit code for this error (see `docs/spec/cli.md`).
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(_) | Error::Schema { .. } => 2,
            Error::DestructiveRefused => 3,
            Error::Drift(_) => 4,
            Error::LockTimeout(_) => 5,
            Error::Connect { .. } => 6,
            Error::Unsupported { .. } => 7,
            _ => 1,
        }
    }
}
