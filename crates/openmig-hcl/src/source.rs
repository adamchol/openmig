use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Index of a file inside a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// A byte range inside one source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: FileId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(file: FileId, start: usize, end: usize) -> Self {
        Self { file, start, end }
    }

    pub fn to_miette(self) -> miette::SourceSpan {
        (self.start, self.end.saturating_sub(self.start)).into()
    }
}

#[derive(Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub content: Arc<str>,
}

/// All files that took part in a load, used to render diagnostics.
#[derive(Debug, Default, Clone)]
pub struct SourceMap {
    files: Vec<Arc<SourceFile>>,
}

impl SourceMap {
    pub fn add(&mut self, path: impl Into<PathBuf>, content: impl Into<Arc<str>>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(Arc::new(SourceFile {
            path: path.into(),
            content: content.into(),
        }));
        id
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn path(&self, id: FileId) -> &Path {
        &self.get(id).path
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f.as_ref()))
    }

    /// 1-based line and column of a byte offset.
    pub fn line_col(&self, id: FileId, offset: usize) -> (usize, usize) {
        let content = &self.get(id).content;
        let offset = offset.min(content.len());
        let before = &content[..offset];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(offset, |nl| offset - nl - 1) + 1;
        (line, col)
    }
}
