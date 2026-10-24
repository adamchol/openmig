//! Changes between two models. The variants are a placeholder for the diff implementation.

use openmig_core::{ChangeInfo, ObjectRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Create(ObjectRef),
    Drop(ObjectRef),
    /// In-place changes, described for summaries.
    Alter {
        object: ObjectRef,
        ops: Vec<String>,
    },
    Rename {
        from: ObjectRef,
        to: ObjectRef,
    },
    /// Drop and create again.
    Recreate(ObjectRef),
}

impl ChangeInfo for Change {
    fn object(&self) -> ObjectRef {
        match self {
            Change::Create(o) | Change::Drop(o) | Change::Recreate(o) => o.clone(),
            Change::Alter { object, .. } => object.clone(),
            Change::Rename { to, .. } => to.clone(),
        }
    }

    fn summary(&self) -> String {
        match self {
            Change::Create(o) => format!("create {o}"),
            Change::Drop(o) => format!("drop {o}"),
            Change::Recreate(o) => format!("recreate {o}"),
            Change::Alter { object, ops } => format!("alter {object}: {}", ops.join(", ")),
            Change::Rename { from, to } => format!("rename {from} to {}", to.dotted()),
        }
    }

    fn destructive(&self) -> Option<String> {
        match self {
            Change::Drop(o) => Some(format!("drops {o}")),
            _ => None,
        }
    }
}
