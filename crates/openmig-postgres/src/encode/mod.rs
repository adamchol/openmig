//! Encoding an inspected [`Database`] as HCL for `openmig inspect`.
//!
//! The output decodes back to the same model ([`crate::decode::decode`]): every value is written
//! in a form the decoder turns into exactly the inspected text, and falls back to a plain string
//! or `sql("...")` when no nicer form round-trips. Attributes at their defaults are left out.
//! Owners, dependencies, identity sequence names and the server version are never written.

mod acl;
mod objects;
mod table;
mod values;

use crate::model::{Database, Function, Schema};
use crate::settings::PgSettings;
use openmig_core::{EncodeOptions, EncodedFile, SplitBy};
use openmig_hcl::Address;
use openmig_hcl::write::{WBlock, WExpr, render};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Encode `model` as HCL files.
pub fn encode(
    model: &Database,
    settings: &PgSettings,
    options: &EncodeOptions,
) -> Vec<EncodedFile> {
    let encoder = Encoder::new(model, settings);
    let mut sections: Vec<(Section, WBlock)> = Vec::new();
    for role in model.roles.values() {
        sections.push((Section::cluster(Kind::Role), encoder.role(role)));
    }
    for extension in model.extensions.values() {
        sections.push((
            Section::cluster(Kind::Extension),
            encoder.extension(extension),
        ));
    }
    for schema in model.schemas.values() {
        encoder.schema(schema, &mut sections);
    }
    for trigger in model.event_triggers.values() {
        sections.push((
            Section::cluster(Kind::EventTrigger),
            encoder.event_trigger(trigger),
        ));
    }
    files(sections, options.split)
}

/// Kinds of top-level blocks, in output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Role,
    Extension,
    Schema,
    Enum,
    Composite,
    Domain,
    Sequence,
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Trigger,
    EventTrigger,
    Grant,
    Data,
}

impl Kind {
    /// File name for `SplitBy::Kind`.
    fn file(self) -> &'static str {
        match self {
            Kind::Role => "roles.hcl",
            Kind::Extension => "extensions.hcl",
            Kind::Schema => "schemas.hcl",
            Kind::Enum => "enums.hcl",
            Kind::Composite => "composites.hcl",
            Kind::Domain => "domains.hcl",
            Kind::Sequence => "sequences.hcl",
            Kind::Table => "tables.hcl",
            Kind::View => "views.hcl",
            Kind::MaterializedView => "materialized_views.hcl",
            Kind::Function => "functions.hcl",
            Kind::Procedure => "procedures.hcl",
            Kind::Trigger => "triggers.hcl",
            Kind::EventTrigger => "event_triggers.hcl",
            Kind::Grant => "grants.hcl",
            Kind::Data => "data.hcl",
        }
    }
}

/// Where a block goes: its schema (`None` for cluster-wide objects) and kind.
#[derive(Clone, Debug)]
struct Section {
    schema: Option<String>,
    kind: Kind,
}

impl Section {
    fn cluster(kind: Kind) -> Self {
        Self { schema: None, kind }
    }

    fn of(schema: &str, kind: Kind) -> Self {
        Self {
            schema: Some(schema.to_owned()),
            kind,
        }
    }
}

/// A file name for a schema: path separators and NUL replaced.
fn schema_file(schema: &str) -> String {
    let safe: String = schema
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = if safe.starts_with('.') {
        format!("_{safe}")
    } else {
        safe
    };
    format!("{safe}.hcl")
}

fn files(sections: Vec<(Section, WBlock)>, split: SplitBy) -> Vec<EncodedFile> {
    let mut grouped: BTreeMap<String, Vec<(Kind, usize, WBlock)>> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut schema_files: BTreeMap<String, String> = BTreeMap::new();
    if split == SplitBy::Schema {
        used.insert("cluster.hcl".to_owned());
    }
    for (i, (section, block)) in sections.into_iter().enumerate() {
        let file = match split {
            SplitBy::None => "schema.hcl".to_owned(),
            SplitBy::Kind => section.kind.file().to_owned(),
            SplitBy::Schema => match &section.schema {
                None => "cluster.hcl".to_owned(),
                Some(schema) => schema_files
                    .entry(schema.clone())
                    .or_insert_with(|| {
                        let base = schema_file(schema);
                        let mut name = base.clone();
                        let mut n = 1;
                        while used.contains(&name) {
                            n += 1;
                            name = format!("{}_{n}.hcl", base.trim_end_matches(".hcl"));
                        }
                        used.insert(name.clone());
                        name
                    })
                    .clone(),
            },
        };
        grouped
            .entry(file)
            .or_default()
            .push((section.kind, i, block));
    }
    if grouped.is_empty() && split == SplitBy::None {
        grouped.insert("schema.hcl".to_owned(), Vec::new());
    }
    grouped
        .into_iter()
        .map(|(path, mut blocks)| {
            if split == SplitBy::Kind {
                blocks.sort_by_key(|(_, i, _)| *i);
            }
            let blocks: Vec<WBlock> = blocks.into_iter().map(|(_, _, b)| b).collect();
            EncodedFile {
                path: PathBuf::from(path),
                content: render(&blocks),
            }
        })
        .collect()
}

/// Labels of functions and procedures by signature: the name, or for overloads the name followed
/// by the argument types (`add_integer_integer`).
fn routine_labels(routines: &BTreeMap<String, Function>) -> BTreeMap<String, String> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for f in routines.values() {
        *count.entry(f.name.as_str()).or_default() += 1;
    }
    let unique = |f: &Function| count[f.name.as_str()] == 1;
    let mut taken: BTreeSet<String> = routines
        .values()
        .filter(|f| unique(f))
        .map(|f| f.name.clone())
        .collect();
    let mut out = BTreeMap::new();
    for (signature, f) in routines {
        if unique(f) {
            out.insert(signature.clone(), f.name.clone());
            continue;
        }
        let mut base = f.name.clone();
        for t in f.input_types() {
            let word: String = t
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        ' '
                    }
                })
                .collect();
            for part in word.split_whitespace() {
                base.push('_');
                base.push_str(part);
            }
        }
        let mut label = base.clone();
        let mut n = 1;
        while taken.contains(&label) {
            n += 1;
            label = format!("{base}_{n}");
        }
        taken.insert(label.clone());
        out.insert(signature.clone(), label);
    }
    out
}

/// Shared state: the model and the labels used for references.
struct Encoder<'a> {
    model: &'a Database,
    settings: &'a PgSettings,
    /// `(schema, signature)` → label of functions and procedures.
    function_labels: BTreeMap<(String, String), String>,
    procedure_labels: BTreeMap<(String, String), String>,
    /// Schemas written as `schema` blocks (and therefore referenceable).
    schema_blocks: BTreeSet<String>,
}

impl<'a> Encoder<'a> {
    fn new(model: &'a Database, settings: &'a PgSettings) -> Self {
        let mut function_labels = BTreeMap::new();
        let mut procedure_labels = BTreeMap::new();
        let mut schema_blocks = BTreeSet::new();
        for schema in model.schemas.values() {
            for (sig, label) in routine_labels(&schema.functions) {
                function_labels.insert((schema.name.clone(), sig), label);
            }
            for (sig, label) in routine_labels(&schema.procedures) {
                procedure_labels.insert((schema.name.clone(), sig), label);
            }
            if Self::needs_schema_block(settings, schema) {
                schema_blocks.insert(schema.name.clone());
            }
        }
        Self {
            model,
            settings,
            function_labels,
            procedure_labels,
            schema_blocks,
        }
    }

    /// Schemas are declared implicitly by their objects; only the default schema and `public`
    /// with default comment and privileges need no block at all.
    fn needs_schema_block(settings: &PgSettings, schema: &Schema) -> bool {
        let implicit = schema.name == settings.default_schema || schema.name == "public";
        let (comment, acl) = crate::decode::schema_defaults(settings, &schema.name);
        !implicit || schema.comment != comment || schema.acl != acl
    }

    /// Label parts of a schema object: `[schema, name]`. A dot cannot be part of the schema
    /// label, so such schemas are written with `schema = ...` and a substitute label.
    fn parts(&self, schema: &str, name: &str) -> Vec<String> {
        vec![schema.replace('.', "_"), name.to_owned()]
    }

    fn address(&self, kind: &str, schema: &str, name: &str) -> Address {
        Address::new(kind, self.parts(schema, name))
    }

    /// A qualified block with its label and, for schemas containing a dot, `schema`.
    fn qualified_block(&self, kind: &str, schema: &str, name: &str) -> WBlock {
        let parts = self.parts(schema, name);
        let mut block = WBlock::new(kind, vec![parts.join(".")]);
        if schema.contains('.') {
            block.push_attr("schema", self.schema_expr(schema));
        }
        block
    }

    /// A reference to a schema block, or the name.
    fn schema_expr(&self, schema: &str) -> WExpr {
        if self.schema_blocks.contains(schema) {
            WExpr::Ref(Address::new("schema", vec![schema.to_owned()]))
        } else {
            WExpr::String(schema.to_owned())
        }
    }

    fn schema(&self, schema: &'a Schema, out: &mut Vec<(Section, WBlock)>) {
        let s = schema.name.as_str();
        if self.schema_blocks.contains(s) {
            out.push((Section::of(s, Kind::Schema), self.schema_block(schema)));
        }
        for e in schema.enums.values() {
            out.push((Section::of(s, Kind::Enum), self.enum_block(schema, e)));
        }
        for c in schema.composites.values() {
            out.push((Section::of(s, Kind::Composite), self.composite(schema, c)));
        }
        for d in schema.domains.values() {
            out.push((Section::of(s, Kind::Domain), self.domain(schema, d)));
        }
        let serial_sequences = self.serial_sequences(schema);
        for q in schema.sequences.values() {
            if !serial_sequences.contains(&q.name) {
                out.push((Section::of(s, Kind::Sequence), self.sequence(schema, q)));
            }
        }
        for t in schema.tables.values() {
            out.push((
                Section::of(s, Kind::Table),
                self.table(schema, t, &serial_sequences),
            ));
        }
        for v in schema.views.values() {
            out.push((Section::of(s, Kind::View), self.view(schema, v)));
        }
        for m in schema.materialized_views.values() {
            out.push((
                Section::of(s, Kind::MaterializedView),
                self.materialized_view(schema, m),
            ));
        }
        for (signature, f) in &schema.functions {
            out.push((
                Section::of(s, Kind::Function),
                self.routine(schema, signature, f, false),
            ));
        }
        for (signature, f) in &schema.procedures {
            out.push((
                Section::of(s, Kind::Procedure),
                self.routine(schema, signature, f, true),
            ));
        }
        for block in self.view_triggers(schema) {
            out.push((Section::of(s, Kind::Trigger), block));
        }
        for block in self.grants(schema) {
            out.push((Section::of(s, Kind::Grant), block));
        }
        for (table, data) in &self.model.data {
            if table.schema == s {
                out.push((Section::of(s, Kind::Data), self.data(table, data)));
            }
        }
    }
}
