use crate::doc::Document;
use crate::load::{LoadError, LoadOptions};
use crate::write::WBlock;
use std::path::PathBuf;

pub(crate) fn load_paths(
    _paths: &[PathBuf],
    _opts: &LoadOptions<'_>,
) -> Result<Document, LoadError> {
    unimplemented!("hcl loading")
}

pub(crate) fn load_sources(
    _sources: Vec<(PathBuf, String)>,
    _opts: &LoadOptions<'_>,
) -> Result<Document, LoadError> {
    unimplemented!("hcl loading")
}

pub(crate) fn render_blocks(_blocks: &[WBlock]) -> String {
    unimplemented!("hcl writing")
}
