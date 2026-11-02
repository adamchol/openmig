//! The contract between the engine-agnostic flows and a database engine.
//!
//! An engine is split in two:
//! * [`Engine`]: pure functions over the engine's typed model — decoding HCL, encoding HCL,
//!   diffing two models and planning SQL — plus constructors for sessions and scratch servers.
//! * [`Session`]: an open connection — inspection, execution, transactions, locks and the
//!   migration history table.
//!
//! The flows in the `openmig` crate are generic over `E: Engine`; the CLI picks the engine with an
//! enum, so there is no dynamic dispatch.

use crate::error::{Error, ExecError};
use crate::exclude::ExcludeSet;
use crate::history::HistoryRow;
use crate::object::{ObjectKind, ObjectRef};
use crate::plan::Plan;
use crate::rename::{RenameCandidate, RenameHints, RenameSet};
use crate::sql::Dialect;
use openmig_hcl::{Document, SchemaSpec, SourceMap, Span};
use serde::de::DeserializeOwned;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

/// Which schemas (PostgreSQL) or databases (ClickHouse) are managed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SchemaFilter {
    /// Every non-system schema/database.
    #[default]
    All,
    Only(Vec<String>),
}

impl SchemaFilter {
    pub fn includes(&self, name: &str) -> bool {
        match self {
            SchemaFilter::All => true,
            SchemaFilter::Only(names) => names.iter().any(|n| n == name),
        }
    }
}

/// How the rows of a data-managed table are reconciled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataSync {
    /// The table contains exactly the declared rows (insert, update, delete).
    Sync,
    /// Missing declared rows are inserted; nothing is updated or deleted.
    Insert,
}

/// A table whose rows are declared in the schema (`data` blocks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataTable {
    /// `[schema, table]` (PostgreSQL) or `[database, table]` (ClickHouse).
    pub path: Vec<String>,
    /// Columns that rows declare values for (key columns included). Only these are inspected
    /// and compared; other columns keep whatever values they have.
    pub columns: Vec<String>,
    /// `None` when the `data` block does not choose; the flows fill in the configured default.
    pub mode: Option<DataSync>,
}

/// What a session inspects and what a plan may touch.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub schemas: SchemaFilter,
    pub exclude: ExcludeSet,
    /// Manage every cluster-wide role (and user) and their memberships. When false, only owned
    /// roles and users are compared, and grants only to them.
    pub manage_roles: bool,
    /// Schema-less objects OpenMig owns.
    pub owned: Owned,
    /// Tables whose rows are managed (`data` blocks).
    pub data_tables: Vec<DataTable>,
    /// Refuse to manage the rows of a table that has more rows than this.
    pub data_max_rows: u64,
}

/// Schema-less objects that OpenMig owns: roles and users (when roles are not all managed), and
/// event triggers, publications and SQL functions (when only some schemas are managed). Owned
/// objects are those the reference side of a comparison has (the schema for `sync`, the
/// migrations for `status --drift`); others on the target are never compared, so they are never
/// planned, dropped or reported as drift.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owned {
    /// Everything except `hidden` (a scratch server, which holds nothing but OpenMig's objects).
    all: bool,
    objects: BTreeSet<ObjectRef>,
    /// Placeholders created in scratch servers for objects the schema names but doesn't manage.
    hidden: BTreeSet<ObjectRef>,
}

impl Owned {
    pub fn only(objects: impl IntoIterator<Item = ObjectRef>) -> Self {
        Self {
            all: false,
            objects: objects.into_iter().collect(),
            hidden: BTreeSet::new(),
        }
    }

    /// The roles named `names`.
    pub fn roles<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Self {
        Self::only(
            names
                .into_iter()
                .map(|n| ObjectRef::new(ObjectKind::Role, [n.as_ref()])),
        )
    }

    /// Everything but `hidden`.
    pub fn all_except(hidden: impl IntoIterator<Item = ObjectRef>) -> Self {
        Self {
            all: true,
            objects: BTreeSet::new(),
            hidden: hidden.into_iter().collect(),
        }
    }

    pub fn contains(&self, kind: ObjectKind, name: &str) -> bool {
        let r = ObjectRef::new(kind, [name]);
        if self.hidden.contains(&r) {
            return false;
        }
        self.all || self.objects.contains(&r)
    }

    pub fn extend(&mut self, other: &Owned) {
        self.all |= other.all;
        self.objects.extend(other.objects.iter().cloned());
        self.hidden.extend(other.hidden.iter().cloned());
    }
}

/// See [`Engine::scratch_prelude`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScratchPrelude {
    pub statements: Vec<String>,
    /// Placeholders among the created objects, which are not OpenMig's and never compared.
    pub hidden: Vec<ObjectRef>,
}

impl Scope {
    /// Whether a role (or ClickHouse user) is compared: all are when roles are managed.
    pub fn owns_role(&self, kind: ObjectKind, name: &str) -> bool {
        (self.manage_roles && !self.owned.hidden.contains(&ObjectRef::new(kind, [name])))
            || self.owned.contains(kind, name)
    }

    /// Whether a schema-less object other than a role (an event trigger, publication or SQL
    /// function) is compared: all are when every schema is managed.
    pub fn owns_global(&self, kind: ObjectKind, name: &str) -> bool {
        self.schemas == SchemaFilter::All || self.owned.contains(kind, name)
    }

    /// The scope for inspecting a scratch server, where everything but `prelude`'s
    /// placeholders belongs to OpenMig.
    pub fn for_scratch(&self, prelude: &ScratchPrelude) -> Scope {
        Scope {
            owned: Owned::all_except(prelude.hidden.iter().cloned()),
            ..self.clone()
        }
    }
}

/// Why a session is opened. Engines use it to pick session settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPurpose {
    /// A database the user owns (`sync`, `status`, `inspect`, `diff`).
    Target,
    /// `deploy`: applies migration files and writes history.
    Deploy,
    /// A scratch server owned by OpenMig. `Session::reset` is only allowed here.
    Scratch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// DDL can run inside a transaction and be rolled back.
    pub transactional_ddl: bool,
    /// `SAVEPOINT` / `ROLLBACK TO SAVEPOINT` are available.
    pub savepoints: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    /// As reported by the server, e.g. `17.11` or `26.5.1.882`.
    pub text: String,
}

/// How a plan will be used; engines may choose different strategies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMode {
    /// `sync` against a database the user owns.
    Sync,
    /// A migration file for `generate`.
    Generate,
    /// Building the desired schema in a scratch database (no concurrent index builds, no data
    /// safety checks).
    Scratch,
}

/// Everything `diff` and `plan` need besides the two models.
pub struct DiffContext<'a, E: Engine> {
    pub settings: &'a E::Settings,
    pub scope: &'a Scope,
    pub renames: &'a RenameSet,
    pub mode: PlanMode,
    /// Allow drop + create for changes that cannot be altered in place (`sync --recreate`).
    pub allow_recreate: bool,
}

/// The schema as declared, decoded into the engine model.
#[derive(Clone, Debug)]
pub struct Desired<M> {
    pub model: M,
    pub hints: RenameHints,
    /// Where each object was declared, for error messages.
    pub origins: HashMap<ObjectRef, Span>,
    pub sources: SourceMap,
}

impl<M> Desired<M> {
    /// Render `message` pointing at the declaration of `object`, if known.
    pub fn locate(&self, object: &ObjectRef) -> Option<(PathBuf, usize, usize)> {
        let span = self.origins.get(object)?;
        let (line, col) = self.sources.line_col(span.file, span.start);
        Some((self.sources.path(span.file).to_path_buf(), line, col))
    }
}

/// How `inspect` splits its output into files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SplitBy {
    /// One file.
    #[default]
    None,
    /// One file per schema/database (plus one for cluster-wide objects).
    Schema,
    /// One file per object kind.
    Kind,
}

#[derive(Clone, Debug, Default)]
pub struct EncodeOptions {
    pub split: SplitBy,
}

/// One HCL file produced by [`Engine::encode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedFile {
    /// Relative path, e.g. `schema.hcl` or `app.hcl`.
    pub path: PathBuf,
    pub content: String,
}

/// Information every engine change exposes to the flows (plan summaries, confirmations).
pub trait ChangeInfo {
    /// The object being changed.
    fn object(&self) -> ObjectRef;
    /// `create table app.users`, `alter column app.users.email: type text -> citext`.
    fn summary(&self) -> String;
    /// Why the change destroys data, when it does.
    fn destructive(&self) -> Option<String>;
}

/// The pure half of an engine.
pub trait Engine: Sized + Send + Sync + 'static {
    /// `postgres` or `clickhouse`; the value of `engine` in `openmig.toml`.
    const NAME: &'static str;
    const DIALECT: Dialect;

    /// Engine-specific settings, deserialized from the database's table in `openmig.toml` after
    /// the common keys are removed.
    type Settings: DeserializeOwned + Default + Clone + Debug + Send + Sync + 'static;
    /// The engine's complete view of a database.
    type Model: Clone + Default + Debug + PartialEq + Send + Sync + 'static;
    type Change: ChangeInfo + Clone + Debug + Send + Sync + 'static;
    type Session: Session<Self>;

    /// Block vocabulary for loading HCL.
    fn schema_spec(settings: &Self::Settings) -> SchemaSpec;

    /// Decode a loaded document into the model. Errors are rendered diagnostics
    /// ([`Error::Schema`]).
    fn decode(doc: &Document, settings: &Self::Settings) -> Result<Desired<Self::Model>, Error>;

    /// Turn a model into HCL files (`openmig inspect`). Must round-trip through
    /// load + [`Engine::decode`] to an equal model after normalization.
    fn encode(
        model: &Self::Model,
        settings: &Self::Settings,
        options: &EncodeOptions,
    ) -> Vec<EncodedFile>;

    /// Remove excluded and out-of-scope objects from a model (applied to the desired model so it
    /// matches what inspection returns).
    fn restrict(model: &mut Self::Model, scope: &Scope);

    /// The schema-less objects a model holds (for [`Scope::owned`]).
    fn owned(model: &Self::Model) -> Vec<ObjectRef>;

    /// What a fresh scratch server needs before migrations are replayed into it: objects that
    /// the schema names or declares but that migrations never create (roles named without being
    /// declared; ClickHouse users declared without `auth`).
    fn scratch_prelude(model: &Self::Model) -> ScratchPrelude {
        let _ = model;
        ScratchPrelude::default()
    }

    /// Tables with managed rows in a decoded model (for [`Scope::data_tables`]).
    fn data_tables(model: &Self::Model) -> Vec<DataTable>;

    /// True when the model contains no objects (used to refuse a first deploy into a non-empty
    /// database).
    fn is_empty(model: &Self::Model) -> bool;

    /// Resolve `renamed_from` hints against the current model.
    fn resolve_hints(
        current: &Self::Model,
        desired: &Self::Model,
        hints: &RenameHints,
    ) -> RenameSet;

    /// Drop + create pairs that look like renames, for interactive confirmation.
    fn rename_candidates(current: &Self::Model, desired: &Self::Model) -> Vec<RenameCandidate>;

    fn diff(
        current: &Self::Model,
        desired: &Self::Model,
        ctx: &DiffContext<'_, Self>,
    ) -> Result<Vec<Self::Change>, Error>;

    fn plan(
        changes: Vec<Self::Change>,
        current: &Self::Model,
        desired: &Self::Model,
        ctx: &DiffContext<'_, Self>,
    ) -> Result<Plan, Error>;

    /// Statement that sets the lock timeout for the rest of the current transaction
    /// (PostgreSQL: `SET LOCAL lock_timeout = '5000ms'`). `None` when the engine has no such
    /// setting.
    fn lock_timeout_sql(timeout: Duration) -> Option<String> {
        let _ = timeout;
        None
    }

    /// Statement that sets the statement timeout for the rest of the current transaction or
    /// session.
    fn statement_timeout_sql(timeout: Duration) -> Option<String> {
        let _ = timeout;
        None
    }

    /// The scratch server to start for this database.
    fn scratch_spec(settings: &Self::Settings, version: Option<u32>) -> openmig_scratch::Spec;

    /// Open a session. `url` is the database URL from config or CLI, or a scratch server URL.
    fn connect(
        url: &str,
        settings: &Self::Settings,
        purpose: SessionPurpose,
    ) -> impl Future<Output = Result<Self::Session, Error>> + Send;
}

/// The connected half of an engine.
pub trait Session<E: Engine>: Send {
    fn capabilities(&self) -> Capabilities;

    fn server_version(&self) -> &ServerVersion;

    /// Read the database's current state within `scope`. The history table is never included.
    fn inspect(&mut self, scope: &Scope) -> impl Future<Output = Result<E::Model, Error>> + Send;

    /// Run one statement.
    fn execute(&mut self, sql: &str) -> impl Future<Output = Result<(), ExecError>> + Send;

    fn begin(&mut self) -> impl Future<Output = Result<(), ExecError>> + Send;

    fn commit(&mut self) -> impl Future<Output = Result<(), ExecError>> + Send;

    fn rollback(&mut self) -> impl Future<Output = Result<(), ExecError>> + Send;

    /// Take the migration lock, waiting up to `wait`. Fails with [`Error::LockTimeout`].
    fn lock(&mut self, wait: Duration) -> impl Future<Output = Result<(), Error>> + Send;

    fn unlock(&mut self) -> impl Future<Output = Result<(), Error>> + Send;

    /// Create the history table if missing (and upgrade it if older).
    fn history_ensure(&mut self) -> impl Future<Output = Result<(), Error>> + Send;

    /// All history rows, ordered by version. Empty when the table does not exist.
    fn history_load(&mut self) -> impl Future<Output = Result<Vec<HistoryRow>, Error>> + Send;

    /// Insert or replace the row for `row.version`. Runs inside the current transaction if one
    /// is open.
    fn history_write(&mut self, row: &HistoryRow)
    -> impl Future<Output = Result<(), Error>> + Send;

    /// Delete the row for `version` (history repair).
    fn history_delete(&mut self, version: &str) -> impl Future<Output = Result<(), Error>> + Send;

    /// Restore session-level settings between migration files (`RESET ALL`).
    fn reset_session(&mut self) -> impl Future<Output = Result<(), ExecError>> + Send;

    /// Scratch sessions only: wipe every database object and role created since the server
    /// started, leaving a pristine empty database.
    fn reset(&mut self) -> impl Future<Output = Result<(), Error>> + Send;
}
