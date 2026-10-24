//! Decoding a loaded HCL document into the desired [`Database`] (see `docs/spec/hcl.md`).
//!
//! Decoding runs in passes over the evaluated blocks:
//!
//! 1. Every addressable block gets its database identity (schema, name and, for routines, the
//!    signature), so references can be resolved in any order.
//! 2. Roles, extensions and schemas, then every schema object. Names PostgreSQL would generate
//!    (serial sequences, unnamed primary keys and indexes) are chosen once every explicitly named
//!    relation is known.
//! 3. Top-level triggers and policies, then `revoke` and `grant` blocks, then `data` blocks.
//!
//! Problems are collected as diagnostics; decoding fails with all of them rendered.

pub(crate) mod acl;
mod data;
mod keywords;
pub(crate) mod names;
pub(crate) mod objects;
pub(crate) mod routine;
pub(crate) mod table;
pub(crate) mod types;

#[cfg(test)]
mod tests;

use crate::model::{
    Acl, AclItem, ArgMode, ColumnRef, Database, Dependency, Index, PUBLIC, Privilege,
    QualifiedName, Schema, Sequence, SequenceOptions, TriggerEnabled, default_acl,
    function_signature,
};
use crate::restrict::is_builtin_role;
use crate::settings::{PgSettings, PublicDefaults};
use keywords::special_role;
use names::{choose_name, index_name_addition, qualified, quote_ident};
use openmig_core::{Desired, Error, ObjectKind, ObjectRef, RenameHint, RenameHints};
use openmig_hcl::decode::suggest;
use openmig_hcl::{
    Address, Block, BlockDecoder, Diagnostic, Diagnostics, Document, Keyword, Span, Value,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use types::{TypeContext, is_type_ref, ref_type, sql_type};

/// Decode `doc` into the desired model. Errors are rendered diagnostics ([`Error::Schema`]).
pub fn decode(doc: &Document, settings: &PgSettings) -> Result<Desired<Database>, Error> {
    let diags = RefCell::new(Diagnostics::default());
    let mut decoder = Decoder::new(doc, settings, &diags);
    decoder.run();
    let Decoder {
        db, origins, hints, ..
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
        model: db,
        hints: RenameHints(hints),
        origins,
        sources: doc.sources.clone(),
    })
}

/// Database identity of an addressable top-level block.
#[derive(Clone, Debug)]
struct Symbol {
    /// Empty for cluster-wide objects.
    schema: String,
    name: String,
    /// `name(arg types)` of functions and procedures.
    signature: Option<String>,
}

impl Symbol {
    fn qualified(&self) -> QualifiedName {
        QualifiedName::new(&self.schema, &self.name)
    }
}

/// A serial column whose sequence is created once every relation name is known.
struct PendingSerial {
    table: QualifiedName,
    column: String,
    data_type: &'static str,
    span: Span,
}

/// An unlabelled index of a table or materialized view.
struct PendingIndex {
    relation: QualifiedName,
    materialized: bool,
    index: Index,
    /// Key names for the generated name (`ChooseIndexColumnNames`).
    key_names: Vec<String>,
    span: Span,
}

/// Where the columns of a column list must come from.
struct ColumnOwner<'a> {
    /// The enclosing table block, for relative references (`column.id`).
    address: Option<&'a Address>,
    relation: &'a QualifiedName,
    /// Declared columns; `None` when unknown (materialized views).
    columns: Option<&'a [String]>,
}

struct Decoder<'a> {
    doc: &'a Document,
    settings: &'a PgSettings,
    diags: &'a RefCell<Diagnostics>,
    symbols: BTreeMap<Address, Symbol>,
    db: Database,
    origins: HashMap<ObjectRef, Span>,
    hints: Vec<RenameHint>,
    serials: Vec<PendingSerial>,
    /// Tables whose primary key is unnamed: named `<table>_pkey` later.
    unnamed_keys: Vec<(QualifiedName, Span)>,
    unnamed_indexes: Vec<PendingIndex>,
    /// Role names used by grants, policies and memberships.
    mentioned_roles: BTreeSet<String>,
}

impl<'a> Decoder<'a> {
    fn new(doc: &'a Document, settings: &'a PgSettings, diags: &'a RefCell<Diagnostics>) -> Self {
        Self {
            doc,
            settings,
            diags,
            symbols: BTreeMap::new(),
            db: Database::default(),
            origins: HashMap::new(),
            hints: Vec::new(),
            serials: Vec::new(),
            unnamed_keys: Vec::new(),
            unnamed_indexes: Vec::new(),
            mentioned_roles: BTreeSet::new(),
        }
    }

    fn run(&mut self) {
        self.collect_symbols();
        let doc = self.doc;
        for block in doc.blocks_of("schema") {
            self.schema_block(block);
        }
        for block in &doc.blocks {
            match block.kind.as_str() {
                "role" => self.role(block),
                "extension" => self.extension(block),
                _ => {}
            }
        }
        for block in &doc.blocks {
            match block.kind.as_str() {
                "enum" => self.enum_type(block),
                "composite" => self.composite(block),
                "domain" => self.domain(block),
                "sequence" => self.sequence(block),
                "table" => self.table(block),
                "view" => self.view(block),
                "materialized_view" => self.materialized_view(block),
                "function" => self.routine(block, false),
                "procedure" => self.routine(block, true),
                "event_trigger" => self.event_trigger(block),
                _ => {}
            }
        }
        let settings = self.settings;
        self.ensure_schema(&settings.default_schema, None);
        self.ensure_schema("public", None);
        self.generated_names();
        for block in &doc.blocks {
            match block.kind.as_str() {
                "trigger" => self.top_level_trigger(block),
                "policy" => self.top_level_policy(block),
                _ => {}
            }
        }
        for block in doc.blocks_of("revoke") {
            self.privileges(block, false);
        }
        for block in doc.blocks_of("grant") {
            self.privileges(block, true);
        }
        for block in doc.blocks_of("data") {
            self.data(block);
        }
        self.normalize_acls();
        self.check_namespaces();
        self.db.external_roles = self
            .mentioned_roles
            .iter()
            .filter(|r| {
                !self.db.roles.contains_key(*r) && special_role(r).is_none() && !is_builtin_role(r)
            })
            .cloned()
            .collect();
        self.db.normalize_dependencies();
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
        for schema in ["public", self.settings.default_schema.as_str()] {
            self.symbols.insert(
                Address::new("schema", vec![schema.to_owned()]),
                Symbol {
                    schema: String::new(),
                    name: schema.to_owned(),
                    signature: None,
                },
            );
        }
        for block in &doc.blocks {
            let Some(address) = &block.address else {
                continue;
            };
            if matches!(block.kind.as_str(), "schema" | "role" | "extension") {
                let name = string_attr(block, "name").unwrap_or_else(|| address.labels[0].clone());
                self.symbols.insert(
                    address.clone(),
                    Symbol {
                        schema: String::new(),
                        name,
                        signature: None,
                    },
                );
            }
        }
        for block in &doc.blocks {
            let Some(address) = &block.address else {
                continue;
            };
            if is_qualified_kind(&block.kind) && !self.symbols.contains_key(address) {
                let qn = self.label_identity(block);
                self.symbols.insert(
                    address.clone(),
                    Symbol {
                        schema: qn.schema,
                        name: qn.name,
                        signature: None,
                    },
                );
            }
        }
        for block in &doc.blocks {
            let Some(address) = &block.address else {
                continue;
            };
            if matches!(block.kind.as_str(), "function" | "procedure") {
                let types: Vec<String> = block
                    .children("arg")
                    .filter(|arg| arg_mode(arg).is_input())
                    .map(|arg| {
                        arg.attr("type")
                            .and_then(|a| self.type_text(&a.value, TypeContext::Routine).ok())
                            .unwrap_or_default()
                    })
                    .collect();
                if let Some(symbol) = self.symbols.get_mut(address) {
                    symbol.signature = Some(function_signature(
                        &symbol.name,
                        types.iter().map(String::as_str),
                    ));
                }
            }
        }
    }

    /// Identity of a qualified block from its label and its `schema` / `name` attributes,
    /// without diagnostics.
    fn label_identity(&self, block: &Block) -> QualifiedName {
        let label = block.labels.first().map_or("", String::as_str);
        let (label_schema, label_name) = match label.split_once('.') {
            Some((s, n)) => (s, n),
            None => (self.settings.default_schema.as_str(), label),
        };
        let schema = match block.attr("schema").map(|a| &a.value) {
            Some(Value::Ref(a)) if a.kind == "schema" => self
                .symbols
                .get(a)
                .map_or_else(|| a.leaf_label().to_owned(), |s| s.name.clone()),
            Some(Value::String(s)) => s.clone(),
            _ => label_schema.to_owned(),
        };
        let name = string_attr(block, "name").unwrap_or_else(|| label_name.to_owned());
        QualifiedName::new(schema, name)
    }

    /// Identity of a qualified block, reading (and checking) its `schema` and `name`.
    fn qualified_name(&self, dec: &mut BlockDecoder<'_>) -> QualifiedName {
        if let Some(attr) = dec.attr("schema") {
            match &attr.value {
                Value::Ref(a) if a.kind == "schema" => {}
                Value::String(s) if !s.is_empty() => {}
                Value::Null => {}
                other => self.error(
                    attr.value_span,
                    format!(
                        "`schema` must be a schema reference or a string, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        let _ = dec.opt::<String>("name");
        self.label_identity(dec.block())
    }

    /// Name of a nested block: its `name` attribute or its label.
    fn nested_name(&self, dec: &mut BlockDecoder<'_>) -> Option<String> {
        let name = dec.opt::<String>("name");
        let name = name.or_else(|| dec.block().labels.first().cloned());
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

    /// The SQL text of a type value, resolving type references.
    fn type_text(&self, value: &Value, ctx: TypeContext) -> Result<String, String> {
        match value {
            Value::Ref(a) if is_type_ref(a) => self
                .symbol(a)
                .map(|s| ref_type(&s.schema, &s.name))
                .ok_or_else(|| format!("`{a}` cannot be used here")),
            Value::Ref(a) => Err(format!(
                "`{a}` is not a type; types can reference {}",
                types::TYPE_KINDS
                    .iter()
                    .map(|k| format!("`{k}.*`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => sql_type(value, ctx),
        }
    }

    /// A required `type` attribute.
    fn type_attr(&self, dec: &mut BlockDecoder<'_>, ctx: TypeContext) -> Option<String> {
        let Some(attr) = dec.attr("type").filter(|a| !matches!(a.value, Value::Null)) else {
            self.error(
                dec.block().header_span,
                format!("`{}` requires the attribute `type`", dec.block().kind),
            );
            return None;
        };
        match self.type_text(&attr.value, ctx) {
            Ok(t) => Some(t),
            Err(message) => {
                self.error(attr.value_span, message);
                None
            }
        }
    }

    /// The database name of the nested block at `address`.
    fn nested_db_name(&self, address: &Address) -> Option<String> {
        let block = self.doc.get(address)?;
        Some(string_attr(block, "name").unwrap_or_else(|| address.leaf_label().to_owned()))
    }

    /// `(table, column)` of a reference to a table column.
    fn column_ref(&self, address: &Address) -> Option<(QualifiedName, String)> {
        if address.kind != "table" || address.nested.len() != 1 || address.nested[0].0 != "column" {
            return None;
        }
        let table = self.symbol(address)?.qualified();
        Some((table, self.nested_db_name(address)?))
    }

    /// Column names from a list of column references and strings.
    fn column_list(
        &self,
        dec: &mut BlockDecoder<'_>,
        attr: &str,
        owner: &ColumnOwner<'_>,
        required: bool,
    ) -> Option<Vec<String>> {
        let Some(a) = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null)) else {
            if required {
                self.error(
                    dec.block().header_span,
                    format!("`{}` requires the attribute `{attr}`", dec.block().kind),
                );
            }
            return None;
        };
        let items = match &a.value {
            Value::List(items) => items.as_slice(),
            other => {
                self.error(
                    a.value_span,
                    format!(
                        "`{attr}` must be a list of column references or names, found {}",
                        other.type_name()
                    ),
                );
                return None;
            }
        };
        let mut out = Vec::new();
        let mut ok = true;
        for item in items {
            match self.column_item(item, owner) {
                Ok(name) => out.push(name),
                Err(message) => {
                    self.error(a.value_span, message);
                    ok = false;
                }
            }
        }
        if let Some(columns) = owner.columns {
            for name in &out {
                if !columns.contains(name) {
                    let mut d = Diagnostic::error(format!(
                        "unknown column `{name}` in `{}`",
                        owner.relation
                    ))
                    .at(a.value_span);
                    if let Some(s) = suggest(name, columns.iter().map(String::as_str)) {
                        d = d.with_help(format!("did you mean `{s}`?"));
                    }
                    self.push(d);
                    ok = false;
                }
            }
        }
        ok.then_some(out)
    }

    fn column_item(&self, item: &Value, owner: &ColumnOwner<'_>) -> Result<String, String> {
        match item {
            Value::String(s) if !s.is_empty() => Ok(s.clone()),
            Value::Ref(a) => {
                let Some((table, column)) = self.column_ref(a) else {
                    return Err(format!("`{a}` is not a column"));
                };
                let same = match owner.address {
                    Some(own) => own == &a.root(),
                    None => &table == owner.relation,
                };
                if !same {
                    return Err(format!(
                        "`{a}` is a column of `{table}`, not of `{}`",
                        owner.relation
                    ));
                }
                Ok(column)
            }
            other => Err(format!(
                "expected a column reference or name, found {}",
                other.type_name()
            )),
        }
    }

    /// A role name from a role reference, a string or a special role keyword.
    fn role_name(&self, value: &Value) -> Result<String, String> {
        match value {
            Value::Ref(a) if a.kind == "role" && a.nested.is_empty() => self
                .symbol(a)
                .map(|s| s.name.clone())
                .ok_or_else(|| format!("unknown role `{a}`")),
            Value::Ident(w) => special_role(w).map(str::to_owned).ok_or_else(|| {
                format!("`{w}` is not a role: use a role reference (`role.{w}`) or a string")
            }),
            Value::String(s) if s.eq_ignore_ascii_case(PUBLIC) => Ok(PUBLIC.to_owned()),
            Value::String(s) if !s.is_empty() => Ok(s.clone()),
            other => Err(format!(
                "expected a role reference, a role name or PUBLIC, found {}",
                other.type_name()
            )),
        }
    }

    /// A list of roles (`to`, `from`, `member_of`), recorded as mentioned.
    fn role_list(&mut self, dec: &mut BlockDecoder<'_>, attr: &str) -> Option<Vec<String>> {
        let a = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null))?;
        let items = match &a.value {
            Value::List(items) => items.as_slice(),
            other => {
                self.error(
                    a.value_span,
                    format!(
                        "`{attr}` must be a list of roles, found {}",
                        other.type_name()
                    ),
                );
                return None;
            }
        };
        let mut out = Vec::new();
        for item in items {
            match self.role_name(item) {
                Ok(name) => {
                    self.mentioned_roles.insert(name.clone());
                    out.push(name);
                }
                Err(message) => self.error(a.value_span, message),
            }
        }
        Some(out)
    }

    /// `schema.name` of a function from a function reference or a `"schema.name"` string.
    fn function_ref(&self, value: &Value) -> Result<QualifiedName, String> {
        match value {
            Value::Ref(a) if a.kind == "function" && a.nested.is_empty() => self
                .symbol(a)
                .map(Symbol::qualified)
                .ok_or_else(|| format!("unknown function `{a}`")),
            Value::String(s) => Ok(match s.split_once('.') {
                Some((schema, name)) if !schema.is_empty() && !name.is_empty() => {
                    QualifiedName::new(schema, name)
                }
                _ => QualifiedName::new(&self.settings.default_schema, s),
            }),
            other => Err(format!(
                "expected a function reference, found {}",
                other.type_name()
            )),
        }
    }

    /// A list of keywords.
    fn keyword_list<K: Keyword>(&self, dec: &mut BlockDecoder<'_>, attr: &str) -> Option<Vec<K>> {
        let a = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null))?;
        let items = match &a.value {
            Value::List(items) => items.as_slice(),
            other => {
                self.error(
                    a.value_span,
                    format!(
                        "`{attr}` must be a list of {}, found {}",
                        K::EXPECTED,
                        other.type_name()
                    ),
                );
                return None;
            }
        };
        let mut out = Vec::new();
        for item in items {
            let parsed = match item {
                Value::Ident(w) | Value::String(w) => K::parse(w),
                _ => None,
            };
            match parsed {
                Some(k) => out.push(k),
                None => {
                    self.error(
                        a.value_span,
                        format!("`{attr}` entries must be one of {}", K::EXPECTED),
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    /// `true`, `false` or a firing-mode keyword.
    fn enabled(&self, dec: &mut BlockDecoder<'_>) -> TriggerEnabled {
        let Some(attr) = dec.attr("enabled") else {
            return TriggerEnabled::Origin;
        };
        match &attr.value {
            Value::Null | Value::Bool(true) => TriggerEnabled::Origin,
            Value::Bool(false) => TriggerEnabled::Disabled,
            Value::Ident(w) | Value::String(w) if TriggerEnabled::parse(w).is_some() => {
                TriggerEnabled::parse(w).unwrap_or_default()
            }
            _ => {
                self.error(
                    attr.value_span,
                    format!("`enabled` must be one of {}", TriggerEnabled::EXPECTED),
                );
                TriggerEnabled::Origin
            }
        }
    }

    /// An object of storage parameters or settings, values as text.
    fn options(&self, dec: &mut BlockDecoder<'_>, attr: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(a) = dec.attr(attr) else {
            return out;
        };
        match &a.value {
            Value::Null => {}
            Value::Object(map) => {
                for (k, v) in map {
                    match types::scalar_text(v) {
                        Ok(Some(text)) => {
                            out.insert(k.clone(), text);
                        }
                        Ok(None) => {}
                        Err(message) => {
                            self.error(a.value_span, format!("`{attr}.{k}`: {message}"))
                        }
                    }
                }
            }
            other => self.error(
                a.value_span,
                format!("`{attr}` must be an object, found {}", other.type_name()),
            ),
        }
        out
    }

    /// `[schema, name]`-style path of the object a reference names, for `depends_on`.
    fn object_ref(&self, address: &Address) -> Option<ObjectRef> {
        let symbol = self.symbol(address)?;
        let kind = match address.kind.as_str() {
            "schema" => return Some(ObjectRef::new(ObjectKind::Schema, [&symbol.name])),
            "role" => return Some(ObjectRef::new(ObjectKind::Role, [&symbol.name])),
            "extension" => return Some(ObjectRef::new(ObjectKind::Extension, [&symbol.name])),
            "function" | "procedure" => {
                let kind = if address.kind == "function" {
                    ObjectKind::Function
                } else {
                    ObjectKind::Procedure
                };
                return Some(ObjectRef::new(
                    kind,
                    [&symbol.schema, symbol.signature.as_ref()?],
                ));
            }
            other => ObjectKind::parse(other)?,
        };
        match address.nested.as_slice() {
            [] => Some(ObjectRef::new(kind, [&symbol.schema, &symbol.name])),
            [(nested, _)] => {
                let nested_kind = match nested.as_str() {
                    "column" => ObjectKind::Column,
                    "index" => ObjectKind::Index,
                    "foreign_key" => ObjectKind::ForeignKey,
                    "unique" => ObjectKind::Unique,
                    "check" => ObjectKind::Check,
                    _ => return None,
                };
                Some(ObjectRef::new(
                    nested_kind,
                    [&symbol.schema, &symbol.name, &self.nested_db_name(address)?],
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
                Value::Ref(a) => {
                    if let Some(to) = self.object_ref(a) {
                        self.db.dependencies.push(Dependency {
                            from: from.clone(),
                            to,
                        });
                    }
                }
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

    /// Records a `renamed_from` hint. `qualified` allows `schema.name` (tables and other schema
    /// objects); otherwise the old name is in the same container as `to`.
    fn renamed_from(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        kind: ObjectKind,
        to: &[&str],
        qualified: bool,
    ) {
        let Some(old) = dec.opt::<String>("renamed_from") else {
            return;
        };
        let mut from: Vec<String> = to[..to.len() - 1].iter().map(|s| (*s).to_owned()).collect();
        match old.split_once('.') {
            Some((schema, name)) if qualified && to.len() == 2 => {
                from = vec![schema.to_owned(), name.to_owned()];
            }
            _ => from.push(old),
        }
        self.hints.push(RenameHint {
            kind,
            from,
            to: to.iter().map(|s| (*s).to_owned()).collect(),
        });
    }

    /// Registers where `object` is declared. Reports a duplicate when it already is.
    fn declare(&mut self, object: ObjectRef, span: Span, instance: bool) -> bool {
        if let Some(first) = self.origins.get(&object) {
            let mut d = Diagnostic::error(format!("{object} is declared more than once"))
                .at(span)
                .with_label("declared again here")
                .with_related(*first, "first declared here");
            if instance {
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

    fn default_acl(&self, kind: ObjectKind) -> Acl {
        object_default_acl(self.settings, kind)
    }

    fn new_schema(&self, name: &str) -> Schema {
        let mut schema = Schema::new(name);
        (schema.comment, schema.acl) = schema_defaults(self.settings, name);
        schema
    }

    /// Declares `name` implicitly when it is not declared yet.
    fn ensure_schema(&mut self, name: &str, span: Option<Span>) {
        if self.db.schemas.contains_key(name) || is_system_schema(name) {
            return;
        }
        let schema = self.new_schema(name);
        self.db.schemas.insert(name.to_owned(), schema);
        if let Some(span) = span {
            self.origins
                .entry(ObjectRef::new(ObjectKind::Schema, [name]))
                .or_insert(span);
        }
    }

    /// The schema an object is declared in, declared implicitly if needed.
    fn schema_mut(&mut self, name: &str, span: Span) -> &mut Schema {
        self.ensure_schema(name, Some(span));
        self.db
            .schemas
            .entry(name.to_owned())
            .or_insert_with(|| Schema::new(name))
    }

    /// Names PostgreSQL generates: serial sequences, then unnamed primary keys, then unnamed
    /// indexes, each avoiding every relation name already taken in its schema.
    fn generated_names(&mut self) {
        let mut taken: BTreeSet<(String, String)> = BTreeSet::new();
        for schema in self.db.schemas.values() {
            for name in relation_names(schema) {
                taken.insert((schema.name.clone(), name));
            }
        }
        for serial in std::mem::take(&mut self.serials) {
            let schema = serial.table.schema.clone();
            let name = choose_name(&serial.table.name, Some(&serial.column), "seq", |n| {
                taken.contains(&(schema.clone(), n.to_owned()))
            });
            taken.insert((schema.clone(), name.clone()));
            let default = format!(
                "nextval({}::regclass)",
                types::quote_literal(&qualified(&schema, &name))
            );
            let Some(s) = self.db.schemas.get_mut(&schema) else {
                continue;
            };
            if let Some(column) = s
                .tables
                .get_mut(&serial.table.name)
                .and_then(|t| t.column_mut(&serial.column))
            {
                column.default = Some(default);
            }
            s.sequences.insert(
                name.clone(),
                Sequence {
                    name: name.clone(),
                    data_type: serial.data_type.to_owned(),
                    options: SequenceOptions::defaults_for(serial.data_type),
                    owned_by: Some(ColumnRef {
                        schema: schema.clone(),
                        table: serial.table.name.clone(),
                        column: serial.column.clone(),
                    }),
                    ..Sequence::default()
                },
            );
            self.declare(
                ObjectRef::new(ObjectKind::Sequence, [&schema, &name]),
                serial.span,
                false,
            );
        }
        for (table, span) in std::mem::take(&mut self.unnamed_keys) {
            let schema = table.schema.clone();
            let name = choose_name(&table.name, None, "pkey", |n| {
                taken.contains(&(schema.clone(), n.to_owned()))
            });
            taken.insert((schema.clone(), name.clone()));
            let Some(t) = self
                .db
                .schemas
                .get_mut(&schema)
                .and_then(|s| s.tables.get_mut(&table.name))
            else {
                continue;
            };
            let Some(pk) = t.primary_key.as_mut() else {
                continue;
            };
            pk.name = name.clone();
            let index = table::constraint_index(&name, pk);
            t.indexes.insert(name.clone(), index);
            self.declare(
                ObjectRef::new(ObjectKind::PrimaryKey, [&schema, &table.name, &name]),
                span,
                false,
            );
        }
        for pending in std::mem::take(&mut self.unnamed_indexes) {
            let schema = pending.relation.schema.clone();
            let addition = index_name_addition(&names::index_column_names(pending.key_names));
            let name = choose_name(&pending.relation.name, Some(&addition), "idx", |n| {
                taken.contains(&(schema.clone(), n.to_owned()))
            });
            taken.insert((schema.clone(), name.clone()));
            let Some(s) = self.db.schemas.get_mut(&schema) else {
                continue;
            };
            let indexes = if pending.materialized {
                s.materialized_views
                    .get_mut(&pending.relation.name)
                    .map(|m| &mut m.indexes)
            } else {
                s.tables
                    .get_mut(&pending.relation.name)
                    .map(|t| &mut t.indexes)
            };
            let Some(indexes) = indexes else {
                continue;
            };
            indexes.insert(
                name.clone(),
                Index {
                    name: name.clone(),
                    ..pending.index
                },
            );
            self.declare(
                ObjectRef::new(ObjectKind::Index, [&schema, &pending.relation.name, &name]),
                pending.span,
                false,
            );
        }
    }

    /// Relations (tables, views, sequences, indexes, composite types) share one namespace per
    /// schema, and so do types (including the row types of tables and views).
    fn check_namespaces(&self) {
        for schema in self.db.schemas.values() {
            let s = schema.name.as_str();
            let mut relations: Vec<ObjectRef> = Vec::new();
            let mut types: Vec<ObjectRef> = Vec::new();
            let r2 = |kind, name: &str| ObjectRef::new(kind, [s, name]);
            for t in schema.tables.values() {
                relations.push(r2(ObjectKind::Table, &t.name));
                types.push(r2(ObjectKind::Table, &t.name));
                for i in t.indexes.values() {
                    let kind = match &t.primary_key {
                        Some(pk) if pk.name == i.name => ObjectKind::PrimaryKey,
                        _ if t.uniques.contains_key(&i.name) => ObjectKind::Unique,
                        _ if t.exclusions.contains_key(&i.name) => ObjectKind::Exclusion,
                        _ => ObjectKind::Index,
                    };
                    relations.push(ObjectRef::new(kind, [s, &t.name, &i.name]));
                }
            }
            for v in schema.views.values() {
                relations.push(r2(ObjectKind::View, &v.name));
                types.push(r2(ObjectKind::View, &v.name));
            }
            for m in schema.materialized_views.values() {
                relations.push(r2(ObjectKind::MaterializedView, &m.name));
                types.push(r2(ObjectKind::MaterializedView, &m.name));
                for i in m.indexes.values() {
                    relations.push(ObjectRef::new(ObjectKind::Index, [s, &m.name, &i.name]));
                }
            }
            for q in schema.sequences.values() {
                relations.push(r2(ObjectKind::Sequence, &q.name));
            }
            for c in schema.composites.values() {
                relations.push(r2(ObjectKind::Composite, &c.name));
                types.push(r2(ObjectKind::Composite, &c.name));
            }
            for e in schema.enums.values() {
                types.push(r2(ObjectKind::Enum, &e.name));
            }
            for d in schema.domains.values() {
                types.push(r2(ObjectKind::Domain, &d.name));
            }
            self.check_unique_names(s, "relation", relations);
            self.check_unique_names(s, "type", types);
        }
    }

    fn check_unique_names(&self, schema: &str, what: &str, mut objects: Vec<ObjectRef>) {
        let span_of = |o: &ObjectRef| self.origins.get(o).copied();
        objects.sort_by_key(|o| (o.name().to_owned(), span_of(o).map(|s| (s.file, s.start))));
        for pair in objects.windows(2) {
            let (first, second) = (&pair[0], &pair[1]);
            if first.name() != second.name() {
                continue;
            }
            let (Some(a), Some(b)) = (span_of(first), span_of(second)) else {
                continue;
            };
            self.push(
                Diagnostic::error(format!(
                    "{second} uses the {what} name `{}` already used by {first} in schema `{schema}`",
                    first.name()
                ))
                .at(b)
                .with_related(a, "first used here"),
            );
        }
    }

    /// Keeps one entry per grantee and privilege: the grantable one when both are present.
    fn normalize_acls(&mut self) {
        crate::restrict::for_each_acl(&mut self.db, &mut |acl| {
            let grantable: Vec<AclItem> = acl.iter().filter(|i| i.grantable).cloned().collect();
            for item in grantable {
                acl.remove(&AclItem {
                    grantable: false,
                    ..item
                });
            }
        });
    }
}

/// The privileges an object of `kind` grants by default (none for `PUBLIC` when
/// `public_defaults = "revoke"`).
pub(crate) fn object_default_acl(settings: &PgSettings, kind: ObjectKind) -> Acl {
    match settings.public_defaults {
        PublicDefaults::Keep => default_acl(kind),
        PublicDefaults::Revoke => Acl::new(),
    }
}

/// Comment and privileges of a schema that does not set them. Every database has `public`, with
/// a comment and `USAGE` for `PUBLIC`.
pub(crate) fn schema_defaults(settings: &PgSettings, name: &str) -> (Option<String>, Acl) {
    let mut acl = Acl::new();
    if name != "public" {
        return (None, acl);
    }
    if settings.public_defaults == PublicDefaults::Keep {
        acl.insert(AclItem::new(PUBLIC, Privilege::Usage));
    }
    (Some("standard public schema".to_owned()), acl)
}

/// Kinds labelled `"schema.name"`.
fn is_qualified_kind(kind: &str) -> bool {
    matches!(
        kind,
        "table"
            | "view"
            | "materialized_view"
            | "function"
            | "procedure"
            | "enum"
            | "composite"
            | "domain"
            | "sequence"
            | "data"
    )
}

/// Schemas every database has and that are never part of a model.
fn is_system_schema(name: &str) -> bool {
    name == "information_schema" || name.starts_with("pg_")
}

/// A string attribute read without marking it used.
fn string_attr(block: &Block, name: &str) -> Option<String> {
    match block.attr(name).map(|a| &a.value) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The mode of an `arg` block, read without diagnostics.
fn arg_mode(block: &Block) -> ArgMode {
    match block.attr("mode").map(|a| &a.value) {
        Some(Value::Ident(w) | Value::String(w)) => ArgMode::parse(w).unwrap_or_default(),
        _ => ArgMode::In,
    }
}

/// Names of every relation of a schema (pg_class entries).
pub(crate) fn relation_names(schema: &Schema) -> Vec<String> {
    let mut out = Vec::new();
    for t in schema.tables.values() {
        out.push(t.name.clone());
        out.extend(t.indexes.keys().cloned());
    }
    out.extend(schema.views.keys().cloned());
    for m in schema.materialized_views.values() {
        out.push(m.name.clone());
        out.extend(m.indexes.keys().cloned());
    }
    out.extend(schema.sequences.keys().cloned());
    out.extend(schema.composites.keys().cloned());
    out
}

/// A quoted name for messages.
fn shown(name: &str) -> String {
    quote_ident(name)
}
