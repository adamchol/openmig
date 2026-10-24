//! Plan steps: statements plus what they need, create and remove, for ordering.

use openmig_core::graph::Priority;
use openmig_core::{ObjectKind, ObjectRef, Statement};

/// Creations and in-place changes run first; drops run after them unless an edge forces them
/// earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    Create = 0,
    Drop = 1,
}

/// Creation ranks, in the order of `docs/spec/architecture.md`.
pub(crate) mod rank {
    pub const RENAME: u16 = 0;
    pub const ROLE: u16 = 10;
    pub const MEMBERSHIP: u16 = 11;
    pub const EXTENSION: u16 = 20;
    pub const SCHEMA: u16 = 30;
    pub const ENUM: u16 = 40;
    pub const COMPOSITE: u16 = 41;
    pub const DOMAIN: u16 = 42;
    pub const SEQUENCE: u16 = 50;
    pub const TABLE: u16 = 60;
    pub const COLUMN: u16 = 61;
    pub const TABLE_ALTER: u16 = 62;
    pub const OWNED_BY: u16 = 63;
    pub const INDEX: u16 = 70;
    pub const CONSTRAINT: u16 = 80;
    pub const FOREIGN_KEY: u16 = 81;
    pub const REPLICA_IDENTITY: u16 = 82;
    pub const VIEW: u16 = 90;
    pub const MATERIALIZED_VIEW: u16 = 100;
    pub const FUNCTION: u16 = 110;
    pub const TRIGGER: u16 = 120;
    pub const EVENT_TRIGGER: u16 = 130;
    pub const RLS: u16 = 140;
    pub const POLICY: u16 = 141;
    pub const GRANT: u16 = 150;
    pub const COMMENT: u16 = 160;
    pub const DATA_DELETE: u16 = 170;
    pub const DATA_UPSERT: u16 = 171;
}

/// Rank of a drop: dependants before what they depend on, roles last.
pub(crate) fn drop_rank(kind: ObjectKind) -> u16 {
    use ObjectKind as K;
    match kind {
        K::Policy => 10,
        K::Trigger => 11,
        K::EventTrigger => 12,
        K::ForeignKey => 20,
        K::PrimaryKey | K::Unique | K::Check | K::Exclusion => 21,
        K::Index => 22,
        K::View => 30,
        K::MaterializedView => 31,
        K::Function | K::Procedure => 40,
        K::Column => 50,
        K::Table => 60,
        K::Sequence => 70,
        K::Domain => 80,
        K::Composite => 81,
        K::Enum => 82,
        K::Schema => 90,
        K::Extension => 95,
        K::Role => 100,
        _ => 50,
    }
}

/// What a step is, beyond its statements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    Other,
    Rename,
    /// Rows of a data-managed table; runs after every other change to that table.
    Data,
    /// `ALTER TYPE ... ADD VALUE` of an enum.
    EnumValues,
}

#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub statements: Vec<Statement>,
    pub phase: Phase,
    pub rank: u16,
    /// Tie-breaker of the priority; unique per step in practice.
    pub key: String,
    /// The object the step is about.
    pub subject: ObjectRef,
    pub kind: StepKind,
    /// Objects that exist with their desired definition after this step.
    pub creates: Vec<ObjectRef>,
    /// Current objects this step removes, or changes in a way their dependants block.
    pub removes: Vec<ObjectRef>,
    /// Current objects whose dependencies this step gives up (removed or redefined objects).
    pub releases: Vec<ObjectRef>,
    /// Objects that must exist (with their desired definition) before this step.
    pub requires: Vec<ObjectRef>,
    /// Objects whose removal, when planned, must happen before this step.
    pub after_removal_of: Vec<ObjectRef>,
    /// Objects whose removal, when planned, must happen after this step.
    pub before_removal_of: Vec<ObjectRef>,
    /// SQL text whose identifiers hint at dependencies (soft edges).
    pub hints: Vec<String>,
    /// Roles this step revokes privileges from (before such a role is dropped).
    pub revokes_from: Vec<String>,
}

impl Step {
    pub fn new(subject: &ObjectRef, phase: Phase, rank: u16, suffix: &str) -> Self {
        Self {
            statements: Vec::new(),
            phase,
            rank,
            key: format!("{}{suffix}", subject.dotted()),
            subject: subject.clone(),
            kind: StepKind::Other,
            creates: Vec::new(),
            removes: Vec::new(),
            releases: Vec::new(),
            requires: Vec::new(),
            after_removal_of: Vec::new(),
            before_removal_of: Vec::new(),
            hints: Vec::new(),
            revokes_from: Vec::new(),
        }
    }

    /// Add a statement with `comment`, attributed to the step's subject.
    pub fn sql(&mut self, sql: impl Into<String>, comment: impl Into<String>) -> &mut Statement {
        self.statements
            .push(Statement::new(sql, comment).origin(self.subject.clone()));
        self.statements.last_mut().expect("just pushed")
    }

    pub fn priority(&self) -> Priority {
        Priority::new(self.phase as u8, self.rank, self.key.clone())
    }
}
