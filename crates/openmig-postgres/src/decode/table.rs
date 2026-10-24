//! `table` blocks: columns, constraints, indexes, policies and triggers.

use super::keywords::{PolicyKind, ReplicaKeyword};
use super::names::{expression_column_name, quote_ident};
use super::types::{TypeContext, default_sql};
use super::{ColumnOwner, Decoder, PendingIndex, PendingSerial, shown};
use crate::model::{
    Check, Column, ConstraintTrigger, Exclusion, ExclusionElement, FkAction, FkMatch, ForeignKey,
    Generated, GeneratedKind, Identity, IdentityGeneration, Index, IndexKey, KeyExpr, NullsOrder,
    PUBLIC, PartitionOf, Policy, PolicyCommand, PrimaryKey, QualifiedName, ReplicaIdentity,
    SequenceOptions, Table, Trigger, TriggerEvent, TriggerLevel, TriggerTiming, Unique,
};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::decode::suggest;
use openmig_hcl::{Attr, Block, BlockDecoder, Diagnostic, Keyword, Number, Value};
use std::collections::{BTreeMap, BTreeSet};

/// SQL text of a collation: a name (quoted as an identifier when needed), text that is already
/// quoted, or raw SQL with `sql()`. The error is the type found.
pub(crate) fn collation_text(value: &Value) -> Result<Option<String>, &'static str> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.contains('"') => Ok(Some(s.clone())),
        Value::String(s) | Value::Ident(s) if !s.is_empty() => Ok(Some(quote_ident(s))),
        Value::Sql(s) => Ok(Some(s.clone())),
        other => Err(other.type_name()),
    }
}

/// `serial` types: the integer type of the column and its sequence.
fn serial_type(word: &str) -> Option<&'static str> {
    match word.to_ascii_lowercase().as_str() {
        "serial" | "serial4" => Some("integer"),
        "bigserial" | "serial8" => Some("bigint"),
        "smallserial" | "serial2" => Some("smallint"),
        _ => None,
    }
}

/// Explicit sequence parameters; the rest take PostgreSQL's defaults.
#[derive(Default)]
pub(crate) struct SequenceParams {
    pub start: Option<i64>,
    pub increment: Option<i64>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub cache: Option<i64>,
    pub cycle: Option<bool>,
}

/// PostgreSQL's sequence parameters for `data_type` (`smallint`, `integer`, else `bigint`):
/// ascending sequences start at their minimum (default 1), descending ones at their maximum
/// (default -1).
pub(crate) fn sequence_options(data_type: &str, p: &SequenceParams) -> SequenceOptions {
    let (type_min, type_max) = match data_type {
        "smallint" => (i64::from(i16::MIN), i64::from(i16::MAX)),
        "integer" => (i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => (i64::MIN, i64::MAX),
    };
    let increment = p.increment.unwrap_or(1);
    let ascending = increment > 0;
    let min_value = p.min.unwrap_or(if ascending { 1 } else { type_min });
    let max_value = p.max.unwrap_or(if ascending { type_max } else { -1 });
    SequenceOptions {
        start: p
            .start
            .unwrap_or(if ascending { min_value } else { max_value }),
        increment,
        min_value,
        max_value,
        cache: p.cache.unwrap_or(1),
        cycle: p.cycle.unwrap_or(false),
    }
}

/// The index that implements a primary key.
pub(crate) fn constraint_index(name: &str, pk: &PrimaryKey) -> Index {
    Index {
        name: name.to_owned(),
        unique: true,
        keys: pk.columns.iter().map(IndexKey::column).collect(),
        include: pk.include.clone(),
        storage_params: pk.storage_params.clone(),
        constraint: true,
        ..Index::default()
    }
}

fn unique_index(u: &Unique) -> Index {
    Index {
        name: u.name.clone(),
        unique: true,
        keys: u.columns.iter().map(IndexKey::column).collect(),
        include: u.include.clone(),
        nulls_distinct: u.nulls_distinct,
        storage_params: u.storage_params.clone(),
        constraint: true,
        ..Index::default()
    }
}

fn exclusion_index(x: &Exclusion) -> Index {
    Index {
        name: x.name.clone(),
        method: x.method.clone(),
        keys: x.elements.iter().map(|e| e.key.clone()).collect(),
        include: x.include.clone(),
        predicate: x.predicate.clone(),
        storage_params: x.storage_params.clone(),
        constraint: true,
        ..Index::default()
    }
}

/// Whether an object attribute has only the given keys; reports the others.
fn check_keys(dec: &BlockDecoder<'_>, attr: &Attr, allowed: &[&str]) -> bool {
    let Value::Object(map) = &attr.value else {
        return true;
    };
    let mut ok = true;
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            let mut d = Diagnostic::error(format!("unknown key `{key}` in `{}`", attr.name))
                .at(attr.value_span);
            if let Some(s) = suggest(key, allowed.iter().copied()) {
                d = d.with_help(format!("did you mean `{s}`?"));
            }
            dec.push(d);
            ok = false;
        }
    }
    ok
}

fn object_bool(dec: &BlockDecoder<'_>, attr: &Attr, key: &str) -> Option<bool> {
    let Value::Object(map) = &attr.value else {
        return None;
    };
    match map.get(key)? {
        Value::Null => None,
        Value::Bool(b) => Some(*b),
        other => {
            dec.error(
                attr.value_span,
                format!(
                    "`{}.{key}` must be a bool, found {}",
                    attr.name,
                    other.type_name()
                ),
            );
            None
        }
    }
}

fn object_int(dec: &BlockDecoder<'_>, attr: &Attr, key: &str) -> Option<i64> {
    let Value::Object(map) = &attr.value else {
        return None;
    };
    match map.get(key)? {
        Value::Null => None,
        Value::Number(Number::Int(i)) => Some(*i),
        other => {
            dec.error(
                attr.value_span,
                format!(
                    "`{}.{key}` must be an integer, found {}",
                    attr.name,
                    other.type_name()
                ),
            );
            None
        }
    }
}

fn object_string(dec: &BlockDecoder<'_>, attr: &Attr, key: &str) -> Option<String> {
    let Value::Object(map) = &attr.value else {
        return None;
    };
    match map.get(key)? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => {
            dec.error(
                attr.value_span,
                format!(
                    "`{}.{key}` must be a string, found {}",
                    attr.name,
                    other.type_name()
                ),
            );
            None
        }
    }
}

impl<'a> Decoder<'a> {
    pub(super) fn table(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let instance = block.each_key.is_some();
        let table_ref = ObjectRef::new(ObjectKind::Table, [&qn.schema, &qn.name]);
        if !self.declare(table_ref.clone(), block.header_span, instance) {
            return;
        }
        let mut table = Table::new(&qn.name);
        table.comment = self.comment(&mut dec);
        self.renamed_from(&mut dec, ObjectKind::Table, &[&qn.schema, &qn.name], true);
        self.depends_on(&mut dec, &table_ref);
        (table.rls_enabled, table.rls_forced) = self.rls(&mut dec);
        table.unlogged = dec.opt("unlogged").unwrap_or(false);
        table.storage_params = self.options(&mut dec, "storage_params");
        table.partition_by = dec.opt::<String>("partition_by").filter(|s| !s.is_empty());
        table.partition_of = self.partition_of(&mut dec);
        table.inherits = self.table_refs(&mut dec, "inherits");

        for mut col in dec.children("column") {
            if let Some(column) = self.column(&mut col, &qn) {
                let r = ObjectRef::new(ObjectKind::Column, [&qn.schema, &qn.name, &column.name]);
                if self.declare(r, col.block().header_span, col.block().each_key.is_some()) {
                    table.columns.push(column);
                }
            }
            col.finish();
        }
        let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let owner = ColumnOwner {
            address: block.address.as_ref(),
            relation: &qn,
            columns: Some(&names),
        };

        let mut keys = dec.children("primary_key");
        for extra in keys.iter().skip(1) {
            self.error(
                extra.block().header_span,
                format!("`{qn}` declares more than one primary key"),
            );
        }
        if let Some(mut pk_dec) = keys.drain(..).next() {
            if let Some(pk) = self.primary_key(&mut pk_dec, &owner) {
                for column in &mut table.columns {
                    if pk.columns.contains(&column.name) {
                        column.not_null = true;
                    }
                }
                let span = pk_dec.block().header_span;
                if pk.name.is_empty() {
                    self.unnamed_keys.push((qn.clone(), span));
                } else {
                    let r =
                        ObjectRef::new(ObjectKind::PrimaryKey, [&qn.schema, &qn.name, &pk.name]);
                    if self.declare(r, span, false) {
                        table
                            .indexes
                            .insert(pk.name.clone(), constraint_index(&pk.name, &pk));
                    }
                }
                table.primary_key = Some(pk);
            }
            pk_dec.finish();
        }

        for mut d in dec.children("foreign_key") {
            if let Some(fk) = self.foreign_key(&mut d, &owner) {
                let r = ObjectRef::new(ObjectKind::ForeignKey, [&qn.schema, &qn.name, &fk.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.foreign_keys.insert(fk.name.clone(), fk);
                }
            }
            d.finish();
        }
        for mut d in dec.children("unique") {
            if let Some(u) = self.unique(&mut d, &owner) {
                let r = ObjectRef::new(ObjectKind::Unique, [&qn.schema, &qn.name, &u.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.indexes.insert(u.name.clone(), unique_index(&u));
                    table.uniques.insert(u.name.clone(), u);
                }
            }
            d.finish();
        }
        for mut d in dec.children("check") {
            if let Some(c) = self.check(&mut d) {
                let r = ObjectRef::new(ObjectKind::Check, [&qn.schema, &qn.name, &c.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.checks.insert(c.name.clone(), c);
                }
            }
            d.finish();
        }
        for mut d in dec.children("exclude") {
            if let Some(x) = self.exclusion(&mut d, &owner) {
                let r = ObjectRef::new(ObjectKind::Exclusion, [&qn.schema, &qn.name, &x.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.indexes.insert(x.name.clone(), exclusion_index(&x));
                    table.exclusions.insert(x.name.clone(), x);
                }
            }
            d.finish();
        }
        for mut d in dec.children("index") {
            if let Some((name, index, key_names)) = self.index(&mut d, &owner) {
                self.add_index(&mut table.indexes, &qn, name, index, key_names, &d, false);
            }
            d.finish();
        }
        for mut d in dec.children("policy") {
            if let Some(p) = self.policy(&mut d) {
                let r = ObjectRef::new(ObjectKind::Policy, [&qn.schema, &qn.name, &p.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.policies.insert(p.name.clone(), p);
                }
            }
            d.finish();
        }
        for mut d in dec.children("trigger") {
            if let Some(t) = self.trigger(&mut d, &owner) {
                let r = ObjectRef::new(ObjectKind::Trigger, [&qn.schema, &qn.name, &t.name]);
                if self.declare(r, d.block().header_span, d.block().each_key.is_some()) {
                    table.triggers.insert(t.name.clone(), t);
                }
            }
            d.finish();
        }
        table.replica_identity = self.replica_identity(&mut dec, &owner);
        dec.finish();
        self.schema_mut(&qn.schema, block.header_span)
            .tables
            .insert(qn.name.clone(), table);
    }

    /// Adds a decoded index, or queues an unlabelled one for naming.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_index(
        &mut self,
        indexes: &mut BTreeMap<String, Index>,
        relation: &QualifiedName,
        name: Option<String>,
        index: Index,
        key_names: Vec<String>,
        dec: &BlockDecoder<'_>,
        materialized: bool,
    ) {
        let span = dec.block().header_span;
        match name {
            Some(name) => {
                let r =
                    ObjectRef::new(ObjectKind::Index, [&relation.schema, &relation.name, &name]);
                if indexes.contains_key(&name) {
                    self.error(
                        span,
                        format!(
                            "index `{name}` of `{relation}` has the name of a constraint of the \
                             same table"
                        ),
                    );
                } else if self.declare(r, span, dec.block().each_key.is_some()) {
                    indexes.insert(name.clone(), Index { name, ..index });
                }
            }
            None => self.unnamed_indexes.push(PendingIndex {
                relation: relation.clone(),
                materialized,
                index,
                key_names,
                span,
            }),
        }
    }

    fn rls(&self, dec: &mut BlockDecoder<'_>) -> (bool, bool) {
        let Some(attr) = dec.attr("rls") else {
            return (false, false);
        };
        match &attr.value {
            Value::Null => (false, false),
            Value::Bool(b) => (*b, false),
            Value::Object(_) => {
                check_keys(dec, attr, &["enabled", "force"]);
                (
                    object_bool(dec, attr, "enabled").unwrap_or(true),
                    object_bool(dec, attr, "force").unwrap_or(false),
                )
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`rls` must be a bool or `{{ enabled = bool, force = bool }}`, found {}",
                        other.type_name()
                    ),
                );
                (false, false)
            }
        }
    }

    fn partition_of(&self, dec: &mut BlockDecoder<'_>) -> Option<PartitionOf> {
        let bound = dec.opt::<String>("partition_bound");
        let Some(attr) = dec
            .attr("partition_of")
            .filter(|a| !matches!(a.value, Value::Null))
        else {
            if bound.is_some() {
                self.error(
                    dec.block().header_span,
                    "`partition_bound` needs `partition_of`",
                );
            }
            return None;
        };
        let parent = match &attr.value {
            Value::Ref(a) if a.kind == "table" && a.nested.is_empty() => {
                self.symbol(a).map(super::Symbol::qualified)
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`partition_of` must be a table reference, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        }?;
        let Some(bound) = bound else {
            self.error(
                dec.block().header_span,
                "`partition_of` needs `partition_bound`, e.g. \"FOR VALUES FROM (1) TO (10)\" or \
                 \"DEFAULT\"",
            );
            return None;
        };
        Some(PartitionOf { parent, bound })
    }

    /// A list of table references.
    pub(super) fn table_refs(&self, dec: &mut BlockDecoder<'_>, attr: &str) -> Vec<QualifiedName> {
        let Some(a) = dec.attr(attr) else {
            return Vec::new();
        };
        let items = match &a.value {
            Value::Null => return Vec::new(),
            Value::List(items) => items.as_slice(),
            other => {
                self.error(
                    a.value_span,
                    format!(
                        "`{attr}` must be a list of table references, found {}",
                        other.type_name()
                    ),
                );
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::Ref(r) if r.kind == "table" && r.nested.is_empty() => {
                    if let Some(s) = self.symbol(r) {
                        out.push(s.qualified());
                    }
                }
                other => self.error(
                    a.value_span,
                    format!(
                        "`{attr}` entries must be table references, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        out
    }

    fn column(&mut self, dec: &mut BlockDecoder<'_>, table: &QualifiedName) -> Option<Column> {
        let name = self.nested_name(dec)?;
        let span = dec.block().header_span;
        let serial = match dec.attr("type").map(|a| &a.value) {
            Some(Value::Ident(w)) => serial_type(w),
            _ => None,
        };
        let data_type = match serial {
            Some(t) => t.to_owned(),
            None => self.type_attr(dec, TypeContext::Column)?,
        };
        let mut column = Column::new(&name, data_type);
        let null: Option<bool> = dec.opt("null");
        column.not_null = null == Some(false);
        let default = dec
            .attr("default")
            .filter(|a| !matches!(a.value, Value::Null));
        let identity = dec
            .attr("identity")
            .filter(|a| !matches!(a.value, Value::Null));
        let generated = dec
            .attr("generated")
            .filter(|a| !matches!(a.value, Value::Null));
        let conflicts = [
            ("identity", identity.is_some()),
            ("generated", generated.is_some()),
            ("default", default.is_some()),
        ];
        let set: Vec<&str> = conflicts
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| *n)
            .collect();
        if set.len() > 1 {
            self.error(
                span,
                format!(
                    "column `{name}` cannot have both `{}` and `{}`",
                    set[0], set[1]
                ),
            );
        }
        if let Some(t) = serial.filter(|_| !set.is_empty()) {
            self.error(
                span,
                format!(
                    "column `{name}` has a serial type, which brings its own default; use \
                     `type = {t}` with `{}`",
                    set[0]
                ),
            );
        }
        if (serial.is_some() || identity.is_some()) && null == Some(true) {
            self.error(
                span,
                format!(
                    "column `{name}` cannot be nullable: identity and serial columns are NOT NULL"
                ),
            );
        }
        if let Some(attr) = default {
            match default_sql(&attr.value, &column.data_type) {
                Ok(text) => column.default = text,
                Err(message) => self.error(attr.value_span, message),
            }
        }
        if let Some(attr) = identity {
            column.identity = self.identity(dec, attr, &column.data_type);
            column.not_null = true;
        }
        if let Some(attr) = generated {
            column.generated = self.generated(dec, attr);
        }
        if let Some(data_type) = serial {
            column.not_null = true;
            self.serials.push(PendingSerial {
                table: table.clone(),
                column: name.clone(),
                data_type,
                span,
            });
        }
        column.collation = self.collation(dec, "collate");
        column.storage = dec.keyword("storage");
        column.compression = match dec.value("compression") {
            Some(Value::Ident(w) | Value::String(w)) => Some(w.to_ascii_lowercase()),
            Some(other) => {
                self.error(
                    span,
                    format!(
                        "`compression` must be a method name, found {}",
                        other.type_name()
                    ),
                );
                None
            }
            None => None,
        };
        column.comment = self.comment(dec);
        self.renamed_from(
            dec,
            ObjectKind::Column,
            &[&table.schema, &table.name, &name],
            false,
        );
        Some(column)
    }

    /// A collation name (quoted as an identifier when needed), or raw SQL with `sql()` or text
    /// that is already quoted.
    pub(super) fn collation(&self, dec: &mut BlockDecoder<'_>, attr: &str) -> Option<String> {
        let a = dec.attr(attr)?;
        match collation_text(&a.value) {
            Ok(text) => text,
            Err(found) => {
                self.error(
                    a.value_span,
                    format!("`{attr}` must be a collation name, found {found}"),
                );
                None
            }
        }
    }

    fn identity(&self, dec: &BlockDecoder<'_>, attr: &Attr, data_type: &str) -> Option<Identity> {
        match &attr.value {
            Value::Ident(w) | Value::String(w) => match IdentityGeneration::parse(w) {
                Some(generation) => Some(Identity {
                    generation,
                    sequence: sequence_options(data_type, &SequenceParams::default()),
                }),
                None => {
                    self.error(
                        attr.value_span,
                        format!("`identity` must be one of {}", IdentityGeneration::EXPECTED),
                    );
                    None
                }
            },
            Value::Object(map) => {
                check_keys(
                    dec,
                    attr,
                    &[
                        "generated",
                        "start",
                        "increment",
                        "min",
                        "max",
                        "cache",
                        "cycle",
                    ],
                );
                let generation = match map.get("generated") {
                    None | Some(Value::Null) => IdentityGeneration::ByDefault,
                    Some(Value::Ident(w) | Value::String(w))
                        if IdentityGeneration::parse(w).is_some() =>
                    {
                        IdentityGeneration::parse(w)?
                    }
                    Some(_) => {
                        self.error(
                            attr.value_span,
                            format!(
                                "`identity.generated` must be one of {}",
                                IdentityGeneration::EXPECTED
                            ),
                        );
                        return None;
                    }
                };
                let params = SequenceParams {
                    start: object_int(dec, attr, "start"),
                    increment: object_int(dec, attr, "increment"),
                    min: object_int(dec, attr, "min"),
                    max: object_int(dec, attr, "max"),
                    cache: object_int(dec, attr, "cache"),
                    cycle: object_bool(dec, attr, "cycle"),
                };
                Some(Identity {
                    generation,
                    sequence: sequence_options(data_type, &params),
                })
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`identity` must be ALWAYS, BY_DEFAULT or an object, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        }
    }

    fn generated(&self, dec: &BlockDecoder<'_>, attr: &Attr) -> Option<Generated> {
        match &attr.value {
            Value::String(s) => Some(Generated {
                expr: s.clone(),
                kind: GeneratedKind::Stored,
            }),
            Value::Object(_) => {
                check_keys(dec, attr, &["expr", "virtual"]);
                let Some(expr) = object_string(dec, attr, "expr") else {
                    self.error(attr.value_span, "`generated` requires `expr`");
                    return None;
                };
                let kind = if object_bool(dec, attr, "virtual").unwrap_or(false) {
                    GeneratedKind::Virtual
                } else {
                    GeneratedKind::Stored
                };
                Some(Generated { expr, kind })
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`generated` must be an expression string or `{{ expr = \"...\", virtual \
                         = bool }}`, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        }
    }

    fn primary_key(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<PrimaryKey> {
        let name = dec
            .opt::<String>("name")
            .or_else(|| dec.block().labels.first().cloned())
            .unwrap_or_default();
        let columns = self.column_list(dec, "columns", owner, true);
        let include = self.column_list(dec, "include", owner, false);
        let pk = PrimaryKey {
            name,
            columns: columns?,
            include: include.unwrap_or_default(),
            deferrable: dec.opt("deferrable").unwrap_or(false),
            initially_deferred: dec.opt("initially_deferred").unwrap_or(false),
            storage_params: self.options(dec, "with"),
            comment: self.comment(dec),
        };
        Some(pk)
    }

    fn foreign_key(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<ForeignKey> {
        let name = self.nested_name(dec)?;
        let columns = self.column_list(dec, "columns", owner, true);
        let references = self.references(dec);
        let on_delete: Option<FkAction> = dec.keyword("on_delete");
        let on_update: Option<FkAction> = dec.keyword("on_update");
        let match_type: Option<FkMatch> = dec.keyword("match");
        let fk = ForeignKey {
            name,
            columns: columns?,
            ref_table: QualifiedName::default(),
            ref_columns: Vec::new(),
            on_update: on_update.unwrap_or_default(),
            on_delete: on_delete.unwrap_or_default(),
            match_type: match_type.unwrap_or_default(),
            deferrable: dec.opt("deferrable").unwrap_or(false),
            initially_deferred: dec.opt("initially_deferred").unwrap_or(false),
            validated: !dec.opt("not_valid").unwrap_or(false),
            enforced: !dec.opt("not_enforced").unwrap_or(false),
            comment: self.comment(dec),
        };
        let (ref_table, ref_columns) = references?;
        if ref_columns.len() != fk.columns.len() {
            self.error(
                dec.block().header_span,
                format!(
                    "foreign key `{}` has {} column(s) but references {}",
                    fk.name,
                    fk.columns.len(),
                    ref_columns.len()
                ),
            );
            return None;
        }
        Some(ForeignKey {
            ref_table,
            ref_columns,
            ..fk
        })
    }

    /// `references`: column references into one table.
    fn references(&self, dec: &mut BlockDecoder<'_>) -> Option<(QualifiedName, Vec<String>)> {
        let Some(attr) = dec
            .attr("references")
            .filter(|a| !matches!(a.value, Value::Null))
        else {
            self.error(
                dec.block().header_span,
                "`foreign_key` requires the attribute `references`",
            );
            return None;
        };
        let Value::List(items) = &attr.value else {
            self.error(
                attr.value_span,
                format!(
                    "`references` must be a list of column references, found {}",
                    attr.value.type_name()
                ),
            );
            return None;
        };
        let mut table: Option<QualifiedName> = None;
        let mut columns = Vec::new();
        for item in items {
            let resolved = match item {
                Value::Ref(a) => self.column_ref(a),
                _ => None,
            };
            let Some((t, c)) = resolved else {
                self.error(
                    attr.value_span,
                    format!(
                        "`references` entries must be column references such as \
                         `table.app.users.column.id`, found {}",
                        item.type_name()
                    ),
                );
                return None;
            };
            match &table {
                Some(first) if first != &t => {
                    self.error(
                        attr.value_span,
                        format!(
                            "`references` must point into one table, found `{first}` and `{t}`"
                        ),
                    );
                    return None;
                }
                _ => table = Some(t),
            }
            columns.push(c);
        }
        let Some(table) = table else {
            self.error(attr.value_span, "`references` cannot be empty");
            return None;
        };
        Some((table, columns))
    }

    fn unique(&mut self, dec: &mut BlockDecoder<'_>, owner: &ColumnOwner<'_>) -> Option<Unique> {
        let name = self.nested_name(dec)?;
        let columns = self.column_list(dec, "columns", owner, true);
        let include = self.column_list(dec, "include", owner, false);
        Some(Unique {
            name,
            columns: columns?,
            include: include.unwrap_or_default(),
            nulls_distinct: dec.opt("nulls_distinct").unwrap_or(true),
            deferrable: dec.opt("deferrable").unwrap_or(false),
            initially_deferred: dec.opt("initially_deferred").unwrap_or(false),
            storage_params: self.options(dec, "with"),
            comment: self.comment(dec),
        })
    }

    fn check(&mut self, dec: &mut BlockDecoder<'_>) -> Option<Check> {
        let name = self.nested_name(dec)?;
        let expr = dec.req::<String>("expr");
        Some(Check {
            name,
            expr: expr?,
            no_inherit: dec.opt("no_inherit").unwrap_or(false),
            validated: !dec.opt("not_valid").unwrap_or(false),
            enforced: !dec.opt("not_enforced").unwrap_or(false),
            comment: self.comment(dec),
        })
    }

    fn exclusion(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<Exclusion> {
        let name = self.nested_name(dec)?;
        let method = self.method(dec);
        let mut elements = Vec::new();
        let mut ok = true;
        for mut key in dec.children("key") {
            let operator = key.req::<String>("operator");
            match (self.index_key(&mut key, owner), operator) {
                (Some((k, _)), Some(operator)) => {
                    elements.push(ExclusionElement { key: k, operator })
                }
                _ => ok = false,
            }
            key.finish();
        }
        if elements.is_empty() && ok {
            self.error(
                dec.block().header_span,
                "`exclude` needs at least one `key` block",
            );
            ok = false;
        }
        let include = self.column_list(dec, "include", owner, false);
        let x = Exclusion {
            name,
            method: method.unwrap_or_else(|| "btree".into()),
            elements,
            include: include.unwrap_or_default(),
            predicate: dec.opt::<String>("where"),
            deferrable: dec.opt("deferrable").unwrap_or(false),
            initially_deferred: dec.opt("initially_deferred").unwrap_or(false),
            storage_params: self.options(dec, "with"),
            comment: self.comment(dec),
        };
        ok.then_some(x)
    }

    /// `using`: an access method name.
    fn method(&self, dec: &mut BlockDecoder<'_>) -> Option<String> {
        match dec.value("using") {
            None => None,
            Some(Value::Ident(w) | Value::String(w)) if !w.is_empty() => {
                Some(w.to_ascii_lowercase())
            }
            Some(other) => {
                self.error(
                    dec.block().header_span,
                    format!(
                        "`using` must be an access method name, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        }
    }

    /// One `key` block: the key part and the name PostgreSQL derives from it.
    fn index_key(
        &self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<(IndexKey, String)> {
        let column = dec
            .attr("column")
            .filter(|a| !matches!(a.value, Value::Null));
        let expr = dec.opt::<String>("expr");
        let (expr, key_name) = match (column, expr) {
            (Some(attr), None) => {
                let name = match self.column_item(&attr.value, owner) {
                    Ok(name) => name,
                    Err(message) => {
                        self.error(attr.value_span, message);
                        return None;
                    }
                };
                if let Some(columns) = owner.columns
                    && !columns.contains(&name)
                {
                    self.error(
                        attr.value_span,
                        format!("unknown column `{name}` in `{}`", owner.relation),
                    );
                    return None;
                }
                (KeyExpr::Column(name.clone()), name)
            }
            (None, Some(expr)) => {
                let name = expression_column_name(&expr);
                (KeyExpr::Expression(expr), name)
            }
            _ => {
                self.error(
                    dec.block().header_span,
                    "a `key` needs exactly one of `column` and `expr`",
                );
                return None;
            }
        };
        let descending: bool = dec.opt("desc").unwrap_or(false);
        let nulls = match dec.keyword::<NullsOrder>("nulls") {
            Some(NullsOrder::First) if !descending => Some(NullsOrder::First),
            Some(NullsOrder::Last) if descending => Some(NullsOrder::Last),
            _ => None,
        };
        let opclass = match dec.value("opclass") {
            None => None,
            Some(Value::Ident(w) | Value::String(w)) if !w.is_empty() => Some(w.clone()),
            Some(other) => {
                self.error(
                    dec.block().header_span,
                    format!(
                        "`opclass` must be an operator class name, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        };
        let collation = self.collation(dec, "collate");
        Some((
            IndexKey {
                expr,
                opclass,
                collation,
                descending,
                nulls,
            },
            key_name,
        ))
    }

    /// An `index` block: its explicit name, the index and the key names for a generated name.
    pub(super) fn index(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<(Option<String>, Index, Vec<String>)> {
        let name = dec
            .opt::<String>("name")
            .or_else(|| dec.block().labels.first().cloned())
            .filter(|n| !n.is_empty());
        let columns = self.column_list(dec, "columns", owner, false);
        let key_blocks = dec.children("key");
        let mut keys = Vec::new();
        let mut key_names = Vec::new();
        let mut ok = true;
        match (columns, key_blocks.is_empty()) {
            (Some(columns), true) => {
                for c in columns {
                    key_names.push(c.clone());
                    keys.push(IndexKey::column(c));
                }
            }
            (None, false) => {
                for mut key in key_blocks {
                    match self.index_key(&mut key, owner) {
                        Some((k, n)) => {
                            keys.push(k);
                            key_names.push(n);
                        }
                        None => ok = false,
                    }
                    key.finish();
                }
            }
            (Some(_), false) => {
                self.error(
                    dec.block().header_span,
                    "an index has either `columns` or `key` blocks, not both",
                );
                ok = false;
            }
            (None, true) => {
                if !dec.has("columns") {
                    self.error(
                        dec.block().header_span,
                        "an index needs `columns` or `key` blocks",
                    );
                }
                ok = false;
            }
        }
        let method = self.method(dec);
        let include = self.column_list(dec, "include", owner, false);
        if let Some(name) = &name {
            self.renamed_from(
                dec,
                ObjectKind::Index,
                &[&owner.relation.schema, &owner.relation.name, name],
                false,
            );
        }
        let index = Index {
            name: name.clone().unwrap_or_default(),
            method: method.unwrap_or_else(|| "btree".into()),
            unique: dec.opt("unique").unwrap_or(false),
            keys,
            include: include.unwrap_or_default(),
            predicate: dec.opt::<String>("where"),
            nulls_distinct: dec.opt("nulls_distinct").unwrap_or(true),
            storage_params: self.options(dec, "with"),
            comment: self.comment(dec),
            constraint: false,
        };
        ok.then_some((name, index, key_names))
    }

    /// A policy (nested in a table or top-level; `on` is read by the caller).
    pub(super) fn policy(&mut self, dec: &mut BlockDecoder<'_>) -> Option<Policy> {
        let name = self.nested_name(dec)?;
        let command: Option<PolicyCommand> = dec.keyword("for");
        let kind: Option<PolicyKind> = dec.keyword("as");
        let mut roles = self
            .role_list(dec, "to")
            .unwrap_or_else(|| vec![PUBLIC.to_owned()]);
        roles.sort();
        roles.dedup();
        if roles.is_empty() {
            roles.push(PUBLIC.to_owned());
        }
        Some(Policy {
            name,
            command: command.unwrap_or_default(),
            permissive: kind != Some(PolicyKind::Restrictive),
            roles,
            using: dec.opt::<String>("using"),
            check: dec.opt::<String>("check"),
            comment: self.comment(dec),
        })
    }

    /// A trigger (nested in a table or top-level; `on` is read by the caller).
    pub(super) fn trigger(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> Option<Trigger> {
        let name = self.nested_name(dec)?;
        let span = dec.block().header_span;
        let mut timing = None;
        let mut events = BTreeSet::new();
        for (attr, t) in [
            ("before", TriggerTiming::Before),
            ("after", TriggerTiming::After),
            ("instead_of", TriggerTiming::InsteadOf),
        ] {
            if let Some(list) = self.keyword_list::<TriggerEvent>(dec, attr) {
                if timing.is_some() {
                    self.error(
                        span,
                        "a trigger has exactly one of `before`, `after` and `instead_of`",
                    );
                }
                timing = Some(t);
                events.extend(list);
            }
        }
        let update_of = self
            .column_list(dec, "update_of", owner, false)
            .unwrap_or_default();
        if !update_of.is_empty() {
            events.insert(TriggerEvent::Update);
        }
        let level: Option<TriggerLevel> = dec.keyword("for");
        let function = match dec
            .attr("execute")
            .filter(|a| !matches!(a.value, Value::Null))
        {
            Some(attr) => match self.function_ref(&attr.value) {
                Ok(f) => Some(f),
                Err(message) => {
                    self.error(attr.value_span, message);
                    None
                }
            },
            None => {
                self.error(span, "`trigger` requires the attribute `execute`");
                None
            }
        };
        let args = self.trigger_args(dec);
        let constraint: bool = dec.opt("constraint").unwrap_or(false);
        let deferrable: bool = dec.opt("deferrable").unwrap_or(false);
        let initially_deferred: bool = dec.opt("initially_deferred").unwrap_or(false);
        let from = match dec.value("from") {
            None => None,
            Some(Value::Ref(a)) if a.kind == "table" && a.nested.is_empty() => {
                self.symbol(a).map(super::Symbol::qualified)
            }
            Some(other) => {
                self.error(
                    span,
                    format!(
                        "`from` must be a table reference, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        };
        if !constraint && (deferrable || initially_deferred || from.is_some()) {
            self.error(
                span,
                "`deferrable`, `initially_deferred` and `from` need `constraint = true`",
            );
        }
        let (mut old_table, mut new_table) = (None, None);
        if let Some(attr) = dec
            .attr("referencing")
            .filter(|a| !matches!(a.value, Value::Null))
        {
            if matches!(attr.value, Value::Object(_)) {
                check_keys(dec, attr, &["old_table", "new_table"]);
                old_table = object_string(dec, attr, "old_table");
                new_table = object_string(dec, attr, "new_table");
            } else {
                self.error(
                    attr.value_span,
                    "`referencing` must be `{ old_table = \"...\", new_table = \"...\" }`",
                );
            }
        }
        let enabled = self.enabled(dec);
        let comment = self.comment(dec);
        let when = dec.opt::<String>("when");
        let Some(timing) = timing else {
            self.error(
                span,
                "a trigger needs one of `before`, `after` or `instead_of`",
            );
            return None;
        };
        let Some(level) = level else {
            self.error(
                span,
                "`trigger` requires the attribute `for` (ROW or STATEMENT)",
            );
            return None;
        };
        Some(Trigger {
            name,
            timing,
            events,
            update_of,
            level,
            when,
            function: function?,
            args,
            enabled,
            constraint: constraint.then_some(ConstraintTrigger {
                deferrable,
                initially_deferred,
                from,
            }),
            old_table,
            new_table,
            comment,
        })
    }

    fn trigger_args(&self, dec: &mut BlockDecoder<'_>) -> Vec<String> {
        let Some(attr) = dec.attr("args") else {
            return Vec::new();
        };
        match &attr.value {
            Value::Null => Vec::new(),
            Value::List(items) => items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    other => {
                        self.error(
                            attr.value_span,
                            format!(
                                "trigger arguments must be strings, found {}",
                                other.type_name()
                            ),
                        );
                        None
                    }
                })
                .collect(),
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`args` must be a list of strings, found {}",
                        other.type_name()
                    ),
                );
                Vec::new()
            }
        }
    }

    fn replica_identity(
        &self,
        dec: &mut BlockDecoder<'_>,
        owner: &ColumnOwner<'_>,
    ) -> ReplicaIdentity {
        let Some(attr) = dec.attr("replica_identity") else {
            return ReplicaIdentity::Default;
        };
        match &attr.value {
            Value::Null => ReplicaIdentity::Default,
            Value::Ident(w) | Value::String(w) if ReplicaKeyword::parse(w).is_some() => {
                match ReplicaKeyword::parse(w) {
                    Some(ReplicaKeyword::Full) => ReplicaIdentity::Full,
                    Some(ReplicaKeyword::Nothing) => ReplicaIdentity::Nothing,
                    _ => ReplicaIdentity::Default,
                }
            }
            Value::String(s) if !s.is_empty() => ReplicaIdentity::Index(s.clone()),
            Value::Ref(a)
                if a.nested.len() == 1
                    && a.nested[0].0 == "index"
                    && owner.address == Some(&a.root()) =>
            {
                ReplicaIdentity::Index(self.nested_db_name(a).unwrap_or_default())
            }
            _ => {
                self.error(
                    attr.value_span,
                    format!(
                        "`replica_identity` must be DEFAULT, FULL, NOTHING or an index of `{}`",
                        shown(&owner.relation.name)
                    ),
                );
                ReplicaIdentity::Default
            }
        }
    }
}
