//! HCL front-end for OpenMig schemas.
//!
//! Loading is a pipeline: parse every file with `hcl-edit` (spans and comments preserved), collect
//! `variable`/`locals` blocks, build a symbol table of referenceable blocks described by the engine's
//! [`SchemaSpec`], expand `for_each`, and evaluate every attribute into a [`Value`]. Engines then decode
//! the resulting [`Document`] with [`BlockDecoder`], which tracks used attributes and reports
//! diagnostics with source spans.

pub mod decode;
pub mod diag;
pub mod doc;
mod eval;
mod functions;
pub mod load;
pub mod source;
pub mod spec;
pub mod value;
pub mod write;

pub use decode::{BlockDecoder, FromValue, Keyword};
pub use diag::{Diagnostic, Diagnostics, Severity};
pub use doc::{Attr, Block, Document};
pub use load::{LoadOptions, VarValue, load, load_sources};
pub use source::{FileId, SourceFile, SourceMap, Span};
pub use spec::{BlockSpec, LabelSpec, SchemaSpec};
pub use value::{Address, Number, Value};
