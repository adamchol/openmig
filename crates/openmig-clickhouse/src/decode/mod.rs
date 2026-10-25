//! Decoding a loaded HCL document into the desired [`Model`] (see `docs/spec/hcl.md`).
//!
//! Decoding runs in passes over the evaluated blocks:
//!
//! 1. Every addressable block gets its database identity (database and name), so references can
//!    be resolved in any order.
//! 2. Databases (the default database always exists), roles and functions, then tables, views,
//!    materialized views and dictionaries.
//! 3. Grants and row policies; `data` blocks are rejected.
//!
//! Values are rendered to SQL text but not canonicalized: the desired model is applied to a
//! scratch server and inspected back before it is compared with anything. Problems are
//! collected as diagnostics; decoding fails with all of them rendered.

mod access;
mod objects;
pub mod values;

#[cfg(test)]
mod tests;

use crate::model::{Database, Dependency, Model};
use crate::settings::{ChSettings, SYSTEM_DATABASES};
use openmig_core::{Desired, Error, ObjectKind, ObjectRef, RenameHint, RenameHints};
use openmig_hcl::{Address, Block, BlockDecoder, Diagnostic, Diagnostics, Document, Span, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Decode `doc` into the desired model. Errors are rendered diagnostics ([`Error::Schema`]).
pub fn decode(doc: &Document, settings: &ChSettings) -> Result<Desired<Model>, Error> {
    let diags = RefCell::new(Diagnostics::default());
    let mut decoder = Decoder::new(doc, settings, &diags);
    decoder.run();
    let Decoder {
        model,
        origins,
        hints,
        ..
    } = decoder;
    let mut diags = diags.into_inner();
    if diags.has_errors() {
        diags
            .items
            .sort_by_key(|d| (d.span.is_none(), d.span.map(|s| (s.file, s.start, s.end))));
        return Err(Error::Schema {
            rendered: diags.render(&doc.sources, false),
        });
    }
    Ok(Desired {
        model,
        hints: RenameHints(hints),
        origins,
        sources: doc.sources.clone(),
    })
}

/// Database identity of an addressable top-level block.
#[derive(Clone, Debug)]
struct Symbol {
    /// Empty for server-wide objects (databases, roles, functions).
    database: String,
    name: String,
}

struct Decoder<'a> {
    doc: &'a Document,
    settings: &'a ChSettings,
    diags: &'a RefCell<Diagnostics>,
    symbols: BTreeMap<Address, Symbol>,
    model: Model,
    origins: HashMap<ObjectRef, Span>,
    hints: Vec<RenameHint>,
    /// Role names used by grants and row policies.
    mentioned_roles: BTreeSet<String>,
    /// `depends_on` edges.
    explicit: Vec<Dependency>,
    /// Relation names per database with their declaration, to reject a table and a view (or
    /// dictionary) of the same name.
    relations: BTreeMap<(String, String), Span>,
}

const QUALIFIED_KINDS: &[&str] = &["table", "view", "materialized_view", "dictionary", "data"];

impl<'a> Decoder<'a> {
    fn new(doc: &'a Document, settings: &'a ChSettings, diags: &'a RefCell<Diagnostics>) -> Self {
        Self {
            doc,
            settings,
            diags,
            symbols: BTreeMap::new(),
            model: Model::default(),
            origins: HashMap::new(),
            hints: Vec::new(),
            mentioned_roles: BTreeSet::new(),
            explicit: Vec::new(),
            relations: BTreeMap::new(),
        }
    }

    fn run(&mut self) {
        self.collect_symbols();
        let doc = self.doc;
        for block in doc.blocks_of("database") {
            self.database(block);
        }
        let default = self.settings.default_database.clone();
        self.ensure_database(&default, None);
        self.ensure_database("default", None);
        for block in &doc.blocks {
            match block.kind.as_str() {
                "role" => self.role(block),
                "function" => self.function(block),
                _ => {}
            }
        }
        for block in &doc.blocks {
            match block.kind.as_str() {
                "table" => self.table(block),
                "view" => self.view(block),
                "materialized_view" => self.materialized_view(block),
                "dictionary" => self.dictionary(block),
                _ => {}
            }
        }
        for block in &doc.blocks {
            match block.kind.as_str() {
                "grant" => self.grant(block),
                "row_policy" => self.row_policy(block),
                "data" => self.error(
                    block.header_span,
                    "`data` blocks are not supported for ClickHouse yet",
                ),
                _ => {}
            }
        }
        self.model.external_roles = self
            .mentioned_roles
            .iter()
            .filter(|r| !self.model.roles.contains_key(*r))
            .cloned()
            .collect();
        let mut dependencies = crate::deps::definition_edges(&self.model);
        dependencies.extend(std::mem::take(&mut self.explicit));
        self.model.dependencies = dependencies;
        self.model.normalize();
    }

    // ----- diagnostics -----

    fn error(&self, span: Span, message: impl Into<String>) {
        self.diags
            .borrow_mut()
            .push(Diagnostic::error(message).at(span));
    }

    fn push(&self, diagnostic: Diagnostic) {
        self.diags.borrow_mut().push(diagnostic);
    }

    fn decoder(&self, block: &'a Block) -> BlockDecoder<'a> {
        BlockDecoder::new(block, self.doc, self.diags)
    }

    // ----- identities -----

    fn collect_symbols(&mut self) {
        let doc = self.doc;
        for block in &doc.blocks {
            let Some(address) = &block.address else {
                continue;
            };
            if matches!(block.kind.as_str(), "database" | "role" | "function") {
                let name = string_attr(block, "name").unwrap_or_else(|| address.labels[0].clone());
                self.symbols.insert(
                    address.clone(),
                    Symbol {
                        database: String::new(),
                        name,
                    },
                );
            }
        }
        for block in &doc.blocks {
            let Some(address) = &block.address else {
                continue;
            };
            if QUALIFIED_KINDS.contains(&block.kind.as_str()) && !self.symbols.contains_key(address)
            {
                let (database, name) = self.label_identity(block);
                self.symbols
                    .insert(address.clone(), Symbol { database, name });
            }
        }
    }

    /// Identity of a qualified block from its label and its `database` / `name` attributes.
    fn label_identity(&self, block: &Block) -> (String, String) {
        let label = block.labels.first().map_or("", String::as_str);
        let (label_db, label_name) = match label.split_once('.') {
            Some((d, n)) => (d, n),
            None => (self.settings.default_database.as_str(), label),
        };
        let database = match block.attr("database").map(|a| &a.value) {
            Some(Value::Ref(a)) if a.kind == "database" => self
                .symbols
                .get(&a.root())
                .map_or_else(|| a.leaf_label().to_owned(), |s| s.name.clone()),
            Some(Value::String(s)) => s.clone(),
            _ => label_db.to_owned(),
        };
        let name = string_attr(block, "name").unwrap_or_else(|| label_name.to_owned());
        (database, name)
    }

    /// Identity of a qualified block, checking its `database` and `name` attributes.
    fn qualified_name(&self, dec: &mut BlockDecoder<'_>) -> (String, String) {
        if let Some(attr) = dec.attr("database") {
            match &attr.value {
                Value::Ref(a) if a.kind == "database" && a.nested.is_empty() => {}
                Value::String(s) if !s.is_empty() => {}
                Value::Null => {}
                other => self.error(
                    attr.value_span,
                    format!(
                        "`database` must be a database reference or a string, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        let _ = dec.opt::<String>("name");
        let (database, name) = self.label_identity(dec.block());
        if SYSTEM_DATABASES.contains(&database.as_str()) {
            self.error(
                dec.block().header_span,
                format!("`{database}` is a system database"),
            );
        }
        (database, name)
    }

    /// The name of a block: its `name` attribute or its label.
    fn block_name(&self, dec: &mut BlockDecoder<'_>) -> Option<String> {
        let name = dec
            .opt::<String>("name")
            .or_else(|| dec.block().labels.first().cloned());
        if name.as_deref().is_some_and(str::is_empty) {
            self.error(dec.block().header_span, "names cannot be empty");
            return None;
        }
        name
    }

    /// The `comment` attribute; an empty comment is no comment.
    fn comment(&self, dec: &mut BlockDecoder<'_>) -> Option<String> {
        dec.opt::<String>("comment").filter(|c| !c.is_empty())
    }

    fn symbol(&self, address: &Address) -> Option<&Symbol> {
        self.symbols.get(&address.root())
    }

    /// The database name of the nested block at `address`.
    fn nested_db_name(&self, address: &Address) -> Option<String> {
        let block = self.doc.get(address)?;
        Some(string_attr(block, "name").unwrap_or_else(|| address.leaf_label().to_owned()))
    }

    /// Registers where `object` is declared. Reports a duplicate when it already is.
    fn declare(&mut self, object: ObjectRef, span: Span) -> bool {
        if let Some(first) = self.origins.get(&object) {
            let mut d = Diagnostic::error(format!("{object} is declared more than once"))
                .at(span)
                .with_label("declared again here")
                .with_related(*first, "first declared here");
            if self.doc.blocks.iter().any(|b| b.each_key.is_some()) {
                d = d.with_help(
                    "blocks created by `for_each` need distinct names: set `name` from `each`",
                );
            }
            self.push(d);
            return false;
        }
        self.origins.insert(object, span);
        true
    }

    /// Registers a table, view, materialized view or dictionary name in its database.
    fn declare_relation(
        &mut self,
        kind: ObjectKind,
        database: &str,
        name: &str,
        span: Span,
    ) -> bool {
        if !self.declare(ObjectRef::new(kind, [database, name]), span) {
            return false;
        }
        let key = (database.to_owned(), name.to_owned());
        if let Some(first) = self.relations.get(&key) {
            self.push(
                Diagnostic::error(format!(
                    "`{database}.{name}` is declared more than once (tables, views, materialized \
                     views and dictionaries share one namespace)"
                ))
                .at(span)
                .with_label("declared again here")
                .with_related(*first, "first declared here"),
            );
            return false;
        }
        self.relations.insert(key, span);
        self.ensure_database(database, Some(span));
        true
    }

    /// Declares the database implicitly (`Atomic`) when it is not declared yet.
    fn ensure_database(&mut self, name: &str, span: Option<Span>) {
        if self.model.databases.contains_key(name) || SYSTEM_DATABASES.contains(&name) {
            return;
        }
        self.model
            .databases
            .insert(name.to_owned(), Database::new(name, "Atomic"));
        if let Some(span) = span {
            self.origins
                .entry(ObjectRef::new(ObjectKind::Database, [name]))
                .or_insert(span);
        }
    }

    fn database_mut(&mut self, name: &str) -> &mut Database {
        self.model
            .databases
            .entry(name.to_owned())
            .or_insert_with(|| Database::new(name, "Atomic"))
    }

    /// The `ObjectRef` of the object a reference names, for `depends_on`.
    fn object_ref(&self, address: &Address) -> Option<ObjectRef> {
        let symbol = self.symbol(address)?;
        let kind = match address.kind.as_str() {
            "database" => return Some(ObjectRef::new(ObjectKind::Database, [&symbol.name])),
            "role" => return Some(ObjectRef::new(ObjectKind::Role, [&symbol.name])),
            "function" => return Some(ObjectRef::new(ObjectKind::Function, [&symbol.name])),
            "table" => ObjectKind::Table,
            "view" => ObjectKind::View,
            "materialized_view" => ObjectKind::MaterializedView,
            "dictionary" => ObjectKind::Dictionary,
            _ => return None,
        };
        match address.nested.as_slice() {
            [] => Some(ObjectRef::new(kind, [&symbol.database, &symbol.name])),
            [(nested, _)] => {
                let nested_kind = match nested.as_str() {
                    "column" => ObjectKind::Column,
                    "index" => ObjectKind::Index,
                    "projection" => ObjectKind::Projection,
                    "constraint" => ObjectKind::Constraint,
                    _ => return None,
                };
                Some(ObjectRef::new(
                    nested_kind,
                    [
                        symbol.database.as_str(),
                        symbol.name.as_str(),
                        &self.nested_db_name(address)?,
                    ],
                ))
            }
            _ => None,
        }
    }

    /// Records `depends_on` references as dependencies of `from`.
    fn depends_on(&mut self, dec: &mut BlockDecoder<'_>, from: &ObjectRef) {
        let Some(attr) = dec.attr("depends_on") else {
            return;
        };
        let items = match &attr.value {
            Value::Null => return,
            Value::List(items) => items,
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`depends_on` must be a list of references, found {}",
                        other.type_name()
                    ),
                );
                return;
            }
        };
        for item in items {
            match item {
                Value::Ref(a) => match self.object_ref(a) {
                    Some(to) => self.explicit.push(Dependency {
                        from: from.clone(),
                        to,
                        soft: false,
                    }),
                    None => self.error(attr.value_span, format!("`{a}` cannot be a dependency")),
                },
                other => self.error(
                    attr.value_span,
                    format!(
                        "`depends_on` entries must be references, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
    }

    /// Records a `renamed_from` hint. For tables the old name may be qualified (`db.name`);
    /// otherwise the old object is in the same container as `to`.
    fn renamed_from(&mut self, dec: &mut BlockDecoder<'_>, kind: ObjectKind, to: &[&str]) {
        let Some(old) = dec.opt::<String>("renamed_from") else {
            return;
        };
        if old.is_empty() {
            self.error(dec.block().header_span, "`renamed_from` cannot be empty");
            return;
        }
        let mut from: Vec<String> = to[..to.len() - 1].iter().map(|s| (*s).to_owned()).collect();
        match old.split_once('.') {
            Some((database, name)) if kind == ObjectKind::Table => {
                from = vec![database.to_owned(), name.to_owned()];
            }
            _ => from.push(old),
        }
        self.hints.push(RenameHint {
            kind,
            from,
            to: to.iter().map(|s| (*s).to_owned()).collect(),
        });
    }

    /// A required SQL-text attribute decoded with `f`.
    fn sql_attr(
        &self,
        dec: &mut BlockDecoder<'_>,
        name: &str,
        required: bool,
        f: fn(&Value) -> Result<String, String>,
    ) -> Option<String> {
        let Some(attr) = dec.attr(name).filter(|a| !matches!(a.value, Value::Null)) else {
            if required {
                self.error(
                    dec.block().header_span,
                    format!("`{}` requires the attribute `{name}`", dec.block().kind),
                );
            }
            return None;
        };
        match f(&attr.value) {
            Ok(text) if text.is_empty() => {
                self.error(attr.value_span, format!("`{name}` cannot be empty"));
                None
            }
            Ok(text) => Some(text),
            Err(expected) => {
                self.error(attr.value_span, format!("`{name}` must be {expected}"));
                None
            }
        }
    }

    /// `settings = { k = v }` with values as SQL literals.
    fn settings_attr(&self, dec: &mut BlockDecoder<'_>, name: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(attr) = dec.attr(name).filter(|a| !matches!(a.value, Value::Null)) else {
            return out;
        };
        let Value::Object(map) = &attr.value else {
            self.error(
                attr.value_span,
                format!(
                    "`{name}` must be an object, found {}",
                    attr.value.type_name()
                ),
            );
            return out;
        };
        for (key, value) in map {
            match values::literal(value) {
                Ok(text) => {
                    out.insert(key.clone(), text);
                }
                Err(expected) => self.error(
                    attr.value_span,
                    format!("setting `{key}` must be {expected}"),
                ),
            }
        }
        out
    }

    /// A list of strings (or identifiers).
    fn string_list(&self, dec: &mut BlockDecoder<'_>, name: &str) -> Option<Vec<String>> {
        let attr = dec.attr(name).filter(|a| !matches!(a.value, Value::Null))?;
        let Value::List(items) = &attr.value else {
            self.error(
                attr.value_span,
                format!("`{name}` must be a list, found {}", attr.value.type_name()),
            );
            return None;
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::String(s) | Value::Ident(s) if !s.is_empty() => out.push(s.clone()),
                Value::Call { .. } | Value::Sql(_) => match values::sql_expr(item) {
                    Ok(s) => out.push(s),
                    Err(e) => self.error(attr.value_span, e),
                },
                other => self.error(
                    attr.value_span,
                    format!(
                        "`{name}` entries must be strings, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        Some(out)
    }
}

fn string_attr(block: &Block, name: &str) -> Option<String> {
    match block.attr(name).map(|a| &a.value) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
