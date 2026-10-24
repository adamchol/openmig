//! Canonical HCL output, used by `openmig inspect` to turn a live database into schema files.
//!
//! Formatting rules: two-space indentation, `=` aligned within a run of consecutive single-line
//! attributes, one blank line between blocks, multi-line strings written as `<<-SQL` heredocs
//! indented with the block, strings that contain `${` or `%{` escaped as `$${` / `%%{`.

use crate::value::Address;

/// An expression to write.
#[derive(Clone, Debug, PartialEq)]
pub enum WExpr {
    Null,
    Bool(bool),
    /// Number text written verbatim.
    Number(String),
    /// A quoted string (escaped as needed). Written as a heredoc when it contains newlines.
    String(String),
    /// A bare identifier: `text`, `CASCADE`.
    Ident(String),
    /// A function-call shaped value: `varchar(255)`, `ZSTD(1)`.
    Call(String, Vec<WExpr>),
    /// A reference: `table.app.users.column.id`.
    Ref(Address),
    /// `sql("...")`.
    Sql(String),
    List(Vec<WExpr>),
    /// An object; keys are written bare when they are identifiers, quoted otherwise.
    Object(Vec<(String, WExpr)>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WItem {
    Attr(String, WExpr),
    Block(WBlock),
    /// A `#` comment line.
    Comment(String),
    /// An explicit blank line.
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WBlock {
    pub kind: String,
    pub labels: Vec<String>,
    pub items: Vec<WItem>,
}

impl WBlock {
    pub fn new(kind: impl Into<String>, labels: Vec<String>) -> Self {
        Self {
            kind: kind.into(),
            labels,
            items: Vec::new(),
        }
    }

    pub fn attr(mut self, name: impl Into<String>, value: WExpr) -> Self {
        self.items.push(WItem::Attr(name.into(), value));
        self
    }

    pub fn block(mut self, block: WBlock) -> Self {
        self.items.push(WItem::Block(block));
        self
    }

    pub fn push_attr(&mut self, name: impl Into<String>, value: WExpr) {
        self.items.push(WItem::Attr(name.into(), value));
    }

    pub fn push_block(&mut self, block: WBlock) {
        self.items.push(WItem::Block(block));
    }
}

/// Render top-level blocks as one HCL file.
pub fn render(blocks: &[WBlock]) -> String {
    crate::eval::render_blocks(blocks)
}
