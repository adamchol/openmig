//! Engine-agnostic building blocks shared by every OpenMig crate.

pub mod engine;
pub mod error;
pub mod exclude;
pub mod graph;
pub mod history;
pub mod object;
pub mod plan;
pub mod rename;
pub mod sql;

pub use engine::{
    Capabilities, ChangeInfo, DataSync, DataTable, Desired, DiffContext, EncodeOptions,
    EncodedFile, Engine, PlanMode, SchemaFilter, Scope, ServerVersion, Session, SessionPurpose,
    SplitBy,
};
pub use error::{Error, ExecError, ExecErrorKind, Result};
pub use exclude::ExcludeSet;
pub use history::{HistoryKind, HistoryRow, HistoryStatus};
pub use object::{ObjectKind, ObjectRef};
pub use plan::{Plan, Statement, TxRequirement};
pub use rename::{RenameCandidate, RenameHint, RenameHints, RenameSet};
pub use sql::Dialect;
