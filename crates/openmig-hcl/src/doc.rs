use crate::diag::Diagnostics;
use crate::source::{SourceMap, Span};
use crate::value::{Address, Value};
use indexmap::IndexMap;

/// An evaluated attribute.
#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub value: Value,
    /// Span of the whole `name = value` attribute.
    pub span: Span,
    /// Span of the value expression.
    pub value_span: Span,
}

/// An evaluated block. `for_each` has been expanded; `variable` and `locals` blocks are removed.
#[derive(Clone, Debug)]
pub struct Block {
    pub kind: String,
    /// Raw labels as written.
    pub labels: Vec<String>,
    /// Canonical identity for referenceable blocks (also set for non-referenceable labelled
    /// blocks so engines can name them consistently).
    pub address: Option<Address>,
    pub attrs: IndexMap<String, Attr>,
    pub blocks: Vec<Block>,
    /// Span of the whole block.
    pub span: Span,
    /// Span of the block header (kind and labels), used to point at a block in diagnostics.
    pub header_span: Span,
    /// Set when the block was produced by `for_each`: the instance key.
    pub each_key: Option<String>,
}

impl Block {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.get(name)
    }

    pub fn children<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
        self.blocks.iter().filter(move |b| b.kind == kind)
    }
}

/// The evaluated schema: every top-level block from every file, in load order.
#[derive(Clone, Debug, Default)]
pub struct Document {
    pub sources: SourceMap,
    pub blocks: Vec<Block>,
    /// Non-fatal diagnostics produced while loading.
    pub warnings: Diagnostics,
}

impl Document {
    pub fn blocks_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
        self.blocks.iter().filter(move |b| b.kind == kind)
    }

    /// Look up a block (top-level or nested) by address.
    pub fn get(&self, address: &Address) -> Option<&Block> {
        let mut block = self.blocks.iter().find(|b| {
            b.address.as_ref().is_some_and(|a| {
                a.nested.is_empty() && a.kind == address.kind && a.labels == address.labels
            })
        })?;
        for (kind, label) in &address.nested {
            block = block
                .blocks
                .iter()
                .find(|b| &b.kind == kind && b.labels.first() == Some(label))?;
        }
        Some(block)
    }
}
