//! Changes between two models. Diffing is not implemented yet; [`Change`] is a placeholder that
//! fixes the engine's types.

use openmig_core::{ChangeInfo, ObjectRef};

/// One change to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Placeholder until diffing lands.
    Unimplemented(ObjectRef),
}

impl ChangeInfo for Change {
    fn object(&self) -> ObjectRef {
        match self {
            Change::Unimplemented(object) => object.clone(),
        }
    }

    fn summary(&self) -> String {
        match self {
            Change::Unimplemented(object) => format!("change {object}"),
        }
    }

    fn destructive(&self) -> Option<String> {
        None
    }
}
