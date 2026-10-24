use crate::object::{ObjectKind, ObjectRef};

/// A `renamed_from` declared in the schema: `to` used to be called `from`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameHint {
    pub kind: ObjectKind,
    /// Full path of the old object, e.g. `["app", "users", "name"]`.
    pub from: Vec<String>,
    /// Full path of the declared object, e.g. `["app", "users", "full_name"]`.
    pub to: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenameHints(pub Vec<RenameHint>);

/// A drop + create pair that might be a rename, offered to the user interactively.
#[derive(Clone, Debug, PartialEq)]
pub struct RenameCandidate {
    pub from: ObjectRef,
    pub to: ObjectRef,
    /// 0.0..=1.0, higher is more likely.
    pub score: f32,
    /// Why this looks like a rename (`same type and nullability`).
    pub reason: String,
}

/// Renames to apply while diffing: each pair maps an object in the current state to its new
/// identity in the desired state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenameSet(pub Vec<(ObjectRef, ObjectRef)>);

impl RenameSet {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, from: ObjectRef, to: ObjectRef) {
        self.0.push((from, to));
    }

    pub fn of_kind(&self, kind: ObjectKind) -> impl Iterator<Item = &(ObjectRef, ObjectRef)> {
        self.0.iter().filter(move |(f, _)| f.kind == kind)
    }
}
