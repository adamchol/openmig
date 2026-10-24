/// How a block kind is labelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelSpec {
    /// `locals { }`, `grant { }`.
    None,
    /// Exactly one label used verbatim: `role "web"`, `column "id"`.
    One,
    /// Zero or one label: an unlabelled nested `index { }` is named by the database.
    OptionalOne,
    /// One label of the form `"namespace.name"`; a label without a dot gets the default namespace.
    /// The label is split on the first dot only.
    Qualified,
}

/// Describes one block kind an engine understands.
#[derive(Clone, Copy, Debug)]
pub struct BlockSpec {
    pub kind: &'static str,
    pub labels: LabelSpec,
    /// Whether the block can be referenced (`table.app.users`, `column.id`).
    pub referenceable: bool,
    /// Several blocks with the same address are merged instead of being an error (`data`).
    pub mergeable: bool,
    pub nested: &'static [BlockSpec],
}

impl BlockSpec {
    pub const fn new(kind: &'static str, labels: LabelSpec) -> Self {
        Self {
            kind,
            labels,
            referenceable: false,
            mergeable: false,
            nested: &[],
        }
    }

    pub const fn referenceable(mut self) -> Self {
        self.referenceable = true;
        self
    }

    pub const fn mergeable(mut self) -> Self {
        self.mergeable = true;
        self
    }

    pub const fn nested(mut self, nested: &'static [BlockSpec]) -> Self {
        self.nested = nested;
        self
    }

    pub fn find_nested(&self, kind: &str) -> Option<&BlockSpec> {
        self.nested.iter().find(|b| b.kind == kind)
    }
}

/// The block vocabulary of an engine.
#[derive(Clone, Debug)]
pub struct SchemaSpec {
    pub blocks: &'static [BlockSpec],
    /// Namespace given to `Qualified` labels without a dot (`public`, `default`).
    pub default_namespace: String,
    /// Referenceable blocks that exist even when no file declares them, e.g. `schema.public`.
    /// References to them resolve; `Document::get` returns `None` for them unless declared.
    pub implicit: Vec<crate::value::Address>,
}

impl SchemaSpec {
    pub fn find(&self, kind: &str) -> Option<&BlockSpec> {
        self.blocks.iter().find(|b| b.kind == kind)
    }
}
