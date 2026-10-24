use crate::diag::Diagnostics;
use crate::doc::Document;
use crate::source::SourceMap;
use crate::spec::SchemaSpec;
use crate::value::Value;
use indexmap::IndexMap;
use std::path::PathBuf;

/// A value for a schema `variable`.
#[derive(Clone, Debug, PartialEq)]
pub enum VarValue {
    /// Text from `--var name=value`; converted to the variable's declared type
    /// (`"false"` becomes a bool for `type = bool`).
    Raw(String),
    /// An already typed value, e.g. from `[databases.x.vars]` in `openmig.toml`.
    Value(Value),
}

pub struct LoadOptions<'a> {
    pub spec: &'a SchemaSpec,
    /// Variable values; later sources must already have been merged by the caller
    /// (CLI over config).
    pub vars: IndexMap<String, VarValue>,
}

/// Loading failed. Carries the sources so the diagnostics can be rendered.
#[derive(Debug)]
pub struct LoadError {
    pub sources: SourceMap,
    pub diagnostics: Diagnostics,
}

impl LoadError {
    pub fn render(&self, color: bool) -> String {
        self.diagnostics.render(&self.sources, color)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(false))
    }
}

impl std::error::Error for LoadError {}

/// Load every `*.hcl` file under `paths` (directories are walked recursively, files are taken as
/// is) and evaluate them as one schema. Files are processed in sorted path order.
pub fn load(paths: &[PathBuf], opts: &LoadOptions<'_>) -> Result<Document, LoadError> {
    crate::eval::load_paths(paths, opts)
}

/// Like [`load`] but from in-memory `(path, content)` pairs. Paths are only used for diagnostics
/// and for resolving `file()` relative to the declaring file.
pub fn load_sources(
    sources: Vec<(PathBuf, String)>,
    opts: &LoadOptions<'_>,
) -> Result<Document, LoadError> {
    crate::eval::load_sources(sources, opts)
}
