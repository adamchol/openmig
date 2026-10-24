use jiff::Timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryKind {
    /// A migration file that was executed.
    Migration,
    /// Recorded by `deploy --baseline`: the file was not executed because the database already
    /// contained its effects.
    Baseline,
}

impl HistoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryKind::Migration => "migration",
            HistoryKind::Baseline => "baseline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "migration" => Some(HistoryKind::Migration),
            "baseline" => Some(HistoryKind::Baseline),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryStatus {
    Applied,
    Failed,
    /// A non-transactional file is being applied (or the process died while applying it).
    Running,
}

impl HistoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryStatus::Applied => "applied",
            HistoryStatus::Failed => "failed",
            HistoryStatus::Running => "running",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "applied" => Some(HistoryStatus::Applied),
            "failed" => Some(HistoryStatus::Failed),
            "running" => Some(HistoryStatus::Running),
            _ => None,
        }
    }
}

/// One row of the `openmig.history` table: the state of one migration file in a database.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRow {
    /// `20261024120000`.
    pub version: String,
    /// Description part of the file name (`add_users`), empty when absent.
    pub name: String,
    /// sha256 hex of the file content.
    pub checksum: String,
    pub kind: HistoryKind,
    pub transactional: bool,
    pub statements_total: u32,
    pub statements_applied: u32,
    /// sha256 hex of each applied statement's text, in order. Used to resume non-transactional
    /// files: a rerun continues after the applied prefix if these still match.
    pub statement_checksums: Vec<String>,
    pub status: HistoryStatus,
    pub error: Option<String>,
    /// 0-based index of the statement that failed.
    pub error_statement: Option<u32>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    /// `openmig/0.1.0 host=<hostname> user=<user>`.
    pub applied_by: String,
}
