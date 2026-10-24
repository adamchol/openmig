//! OpenMig: the flows behind each command, generic over the database engine.

pub mod cli;
pub mod context;
pub mod flows;
pub mod ui;

pub use context::{Context, EngineKind};
pub use ui::Ui;
