//! `table` blocks with their columns, constraints, indexes, policies and triggers.

use super::Encoder;
use super::values::{
    column_ref, columns_expr, enabled, ident, keyword, number, options, strings, word,
};
use crate::decode::names::{make_object_name, qualified};
use crate::decode::relation_names;
use crate::decode::table::{SequenceParams, sequence_options};
use crate::decode::types::{TypeContext, quote_literal};
use crate::model::{
    Column, FkAction, FkMatch, GeneratedKind, Identity, IdentityGeneration, Index, IndexKey,
    KeyExpr, NullsOrder, PUBLIC, Policy, PolicyCommand, ReplicaIdentity, Schema, SequenceOptions,
    Table, Trigger, TriggerLevel, TriggerTiming,
};
use openmig_hcl::Address;
use openmig_hcl::write::{WBlock, WExpr, WItem};
use std::collections::BTreeSet;

/// Explicit identity parameters: those that differ from what the decoder derives.
fn identity_params(data_type: &str, o: &SequenceOptions) -> Vec<(String, WExpr)> {
    let mut out = Vec::new();
    let mut p = SequenceParams::default();
    if o.increment != 1 {
        p.increment = Some(o.increment);
        out.push(("increment".to_owned(), number(o.increment)));
    }
    let d = sequence_options(data_type, &p);
    if o.min_value != d.min_value {
        p.min = Some(o.min_value);
        out.push(("min".to_owned(), number(o.min_value)));
    }
    if o.max_value != d.max_value {
        p.max = Some(o.max_value);
        out.push(("max".to_owned(), number(o.max_value)));
    }
    let d = sequence_options(data_type, &p);
    if o.start != d.start {
        out.push(("start".to_owned(), number(o.start)));
    }
    if o.cache != 1 {
        out.push(("cache".to_owned(), number(o.cache)));
    }
    if o.cycle {
        out.push(("cycle".to_owned(), WExpr::Bool(true)));
    }
    out
}

fn generation(g: IdentityGeneration) -> &'static str {
    match g {
        IdentityGeneration::Always => "ALWAYS",
        IdentityGeneration::ByDefault => "BY_DEFAULT",
    }
}

impl Encoder<'_> {
    /// Sequences of `schema` that the decoder recreates from a `serial` column type: owned by an
    /// integer column whose default is `nextval` of the sequence, named as PostgreSQL names
    /// serial sequences, with default parameters, no comment and default privileges.
    pub(super) fn serial_sequences(&self, schema: &Schema) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for q in schema.sequences.values() {
            let Some(owner) = &q.owned_by else {
                continue;
            };
            if owner.schema != schema.name
                || q.comment.is_some()
                || q.acl
                    != crate::decode::object_default_acl(
                        self.settings,
                        openmig_core::ObjectKind::Sequence,
                    )
                || q.options != SequenceOptions::defaults_for(&q.data_type)
                || q.name != make_object_name(&owner.table, Some(&owner.column), "seq")
            {
                continue;
            }
            let Some(column) = schema
                .tables
                .get(&owner.table)
                .and_then(|t| t.column(&owner.column))
            else {
                continue;
            };
            let default = format!(
                "nextval({}::regclass)",
                quote_literal(&qualified(&schema.name, &q.name))
            );
            if column.data_type == q.data_type
                && matches!(q.data_type.as_str(), "smallint" | "integer" | "bigint")
                && column.not_null
                && column.identity.is_none()
                && column.generated.is_none()
                && column.default.as_deref() == Some(default.as_str())
            {
                out.insert(q.name.clone());
            }
        }
        out
    }

    pub(super) fn table(&self, schema: &Schema, t: &Table, serials: &BTreeSet<String>) -> WBlock {
        let s = schema.name.as_str();
        let mut b = self.qualified_block("table", s, &t.name);
        if let Some(c) = &t.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        match (t.rls_enabled, t.rls_forced) {
            (false, false) => {}
            (true, false) => b.push_attr("rls", WExpr::Bool(true)),
            (enabled, force) => b.push_attr(
                "rls",
                WExpr::Object(vec![
                    ("enabled".into(), WExpr::Bool(enabled)),
                    ("force".into(), WExpr::Bool(force)),
                ]),
            ),
        }
        match &t.replica_identity {
            ReplicaIdentity::Default => {}
            ReplicaIdentity::Full => b.push_attr("replica_identity", ident("FULL")),
            ReplicaIdentity::Nothing => b.push_attr("replica_identity", ident("NOTHING")),
            ReplicaIdentity::Index(name) => {
                let standalone = t.indexes.get(name).is_some_and(|i| !i.constraint);
                let value = if standalone {
                    WExpr::Ref(Address::new("index", vec![name.clone()]))
                } else {
                    WExpr::String(name.clone())
                };
                b.push_attr("replica_identity", value);
            }
        }
        if t.unlogged {
            b.push_attr("unlogged", WExpr::Bool(true));
        }
        if !t.storage_params.is_empty() {
            b.push_attr("storage_params", options(&t.storage_params));
        }
        if let Some(p) = &t.partition_by {
            b.push_attr("partition_by", WExpr::String(p.clone()));
        }
        if let Some(p) = &t.partition_of {
            b.push_attr(
                "partition_of",
                self.table_expr(&p.parent.schema, &p.parent.name),
            );
            b.push_attr("partition_bound", WExpr::String(p.bound.clone()));
        }
        if !t.inherits.is_empty() {
            b.push_attr(
                "inherits",
                WExpr::List(
                    t.inherits
                        .iter()
                        .map(|q| self.table_expr(&q.schema, &q.name))
                        .collect(),
                ),
            );
        }
        let pk_columns: Vec<&String> = t
            .primary_key
            .iter()
            .flat_map(|pk| pk.columns.iter())
            .collect();
        for c in &t.columns {
            let serial = c
                .default
                .as_ref()
                .and_then(|_| {
                    serials.iter().find(|q| {
                        schema.sequences[*q]
                            .owned_by
                            .as_ref()
                            .is_some_and(|o| o.table == t.name && o.column == c.name)
                    })
                })
                .is_some();
            b.push_block(self.column(c, pk_columns.contains(&&c.name), serial));
        }
        let taken = relation_names(schema);
        if let Some(pk) = &t.primary_key {
            let default_name = make_object_name(&t.name, None, "pkey");
            let unique_name = taken.iter().filter(|n| **n == pk.name).count() <= 1;
            let labels = if pk.name == default_name && unique_name {
                Vec::new()
            } else {
                vec![pk.name.clone()]
            };
            let mut k = WBlock::new("primary_key", labels);
            k.push_attr("columns", columns_expr(&pk.columns));
            if !pk.include.is_empty() {
                k.push_attr("include", columns_expr(&pk.include));
            }
            push_deferrable(&mut k, pk.deferrable, pk.initially_deferred);
            if !pk.storage_params.is_empty() {
                k.push_attr("with", options(&pk.storage_params));
            }
            if let Some(c) = &pk.comment {
                k.push_attr("comment", WExpr::String(c.clone()));
            }
            b.push_block(k);
        }
        for fk in t.foreign_keys.values() {
            let refs: Option<Vec<WExpr>> = fk
                .ref_columns
                .iter()
                .map(|c| self.column_expr(&fk.ref_table.schema, &fk.ref_table.name, c))
                .collect();
            let Some(refs) = refs else {
                b.items.push(WItem::Comment(format!(
                    "foreign key {} references {}, which is not part of the inspected schema",
                    fk.name, fk.ref_table
                )));
                continue;
            };
            let mut k = WBlock::new("foreign_key", vec![fk.name.clone()]);
            k.push_attr("columns", columns_expr(&fk.columns));
            k.push_attr("references", WExpr::List(refs));
            if fk.on_update != FkAction::NoAction {
                k.push_attr("on_update", keyword(fk.on_update.as_sql()));
            }
            if fk.on_delete != FkAction::NoAction {
                k.push_attr("on_delete", keyword(fk.on_delete.as_sql()));
            }
            if fk.match_type != FkMatch::Simple {
                k.push_attr("match", keyword(fk.match_type.as_sql()));
            }
            push_deferrable(&mut k, fk.deferrable, fk.initially_deferred);
            push_validity(&mut k, fk.validated, fk.enforced);
            if let Some(c) = &fk.comment {
                k.push_attr("comment", WExpr::String(c.clone()));
            }
            b.push_block(k);
        }
        for u in t.uniques.values() {
            let mut k = WBlock::new("unique", vec![u.name.clone()]);
            k.push_attr("columns", columns_expr(&u.columns));
            if !u.include.is_empty() {
                k.push_attr("include", columns_expr(&u.include));
            }
            if !u.nulls_distinct {
                k.push_attr("nulls_distinct", WExpr::Bool(false));
            }
            push_deferrable(&mut k, u.deferrable, u.initially_deferred);
            if !u.storage_params.is_empty() {
                k.push_attr("with", options(&u.storage_params));
            }
            if let Some(c) = &u.comment {
                k.push_attr("comment", WExpr::String(c.clone()));
            }
            b.push_block(k);
        }
        for c in t.checks.values() {
            let mut k = WBlock::new("check", vec![c.name.clone()]);
            k.push_attr("expr", WExpr::String(c.expr.clone()));
            if c.no_inherit {
                k.push_attr("no_inherit", WExpr::Bool(true));
            }
            push_validity(&mut k, c.validated, c.enforced);
            if let Some(comment) = &c.comment {
                k.push_attr("comment", WExpr::String(comment.clone()));
            }
            b.push_block(k);
        }
        for x in t.exclusions.values() {
            let mut k = WBlock::new("exclude", vec![x.name.clone()]);
            if x.method != "btree" {
                k.push_attr("using", word(&x.method));
            }
            for e in &x.elements {
                let mut key = self.index_key(&e.key, true);
                key.push_attr("operator", WExpr::String(e.operator.clone()));
                k.push_block(key);
            }
            if !x.include.is_empty() {
                k.push_attr("include", columns_expr(&x.include));
            }
            if let Some(p) = &x.predicate {
                k.push_attr("where", WExpr::String(p.clone()));
            }
            push_deferrable(&mut k, x.deferrable, x.initially_deferred);
            if !x.storage_params.is_empty() {
                k.push_attr("with", options(&x.storage_params));
            }
            if let Some(c) = &x.comment {
                k.push_attr("comment", WExpr::String(c.clone()));
            }
            b.push_block(attributes_first(k));
        }
        for index in t.standalone_indexes() {
            b.push_block(self.index(index, true));
        }
        for p in t.policies.values() {
            b.push_block(self.policy(p));
        }
        for g in t.triggers.values() {
            b.push_block(self.trigger(g, None));
        }
        b
    }

    pub(super) fn table_expr(&self, schema: &str, name: &str) -> WExpr {
        WExpr::Ref(self.address("table", schema, name))
    }

    fn column(&self, c: &Column, in_primary_key: bool, serial: bool) -> WBlock {
        let mut b = WBlock::new("column", vec![c.name.clone()]);
        if serial {
            let t = match c.data_type.as_str() {
                "integer" => "serial",
                "bigint" => "bigserial",
                _ => "smallserial",
            };
            b.push_attr("type", ident(t));
        } else {
            b.push_attr("type", self.type_expr(&c.data_type, TypeContext::Column));
        }
        let implied = in_primary_key || serial || c.identity.is_some();
        if c.not_null && !implied {
            b.push_attr("null", WExpr::Bool(false));
        }
        if let (Some(d), false) = (&c.default, serial) {
            b.push_attr("default", self.default_expr(d, &c.data_type));
        }
        if let Some(Identity {
            generation: g,
            sequence,
        }) = &c.identity
        {
            let params = identity_params(&c.data_type, sequence);
            if params.is_empty() {
                b.push_attr("identity", ident(generation(*g)));
            } else {
                let mut entries = vec![("generated".to_owned(), ident(generation(*g)))];
                entries.extend(params);
                b.push_attr("identity", WExpr::Object(entries));
            }
        }
        if let Some(g) = &c.generated {
            let value = match g.kind {
                GeneratedKind::Stored => WExpr::String(g.expr.clone()),
                GeneratedKind::Virtual => WExpr::Object(vec![
                    ("expr".into(), WExpr::String(g.expr.clone())),
                    ("virtual".into(), WExpr::Bool(true)),
                ]),
            };
            b.push_attr("generated", value);
        }
        if let Some(collation) = &c.collation {
            b.push_attr("collate", self.collation_expr(collation));
        }
        if let Some(storage) = c.storage {
            b.push_attr("storage", ident(storage.as_sql()));
        }
        if let Some(compression) = &c.compression {
            b.push_attr("compression", word(compression));
        }
        if let Some(comment) = &c.comment {
            b.push_attr("comment", WExpr::String(comment.clone()));
        }
        b
    }

    /// A `key` block. `refs` writes columns as references (tables) rather than names
    /// (materialized views, whose columns are not declared).
    pub(super) fn index_key(&self, k: &IndexKey, refs: bool) -> WBlock {
        let mut b = WBlock::new("key", Vec::new());
        match &k.expr {
            KeyExpr::Column(c) if refs => b.push_attr("column", column_ref(c)),
            KeyExpr::Column(c) => b.push_attr("column", WExpr::String(c.clone())),
            KeyExpr::Expression(e) => b.push_attr("expr", WExpr::String(e.clone())),
        }
        if k.descending {
            b.push_attr("desc", WExpr::Bool(true));
        }
        match k.nulls {
            Some(NullsOrder::First) => b.push_attr("nulls", ident("FIRST")),
            Some(NullsOrder::Last) => b.push_attr("nulls", ident("LAST")),
            None => {}
        }
        if let Some(o) = &k.opclass {
            b.push_attr("opclass", word(o));
        }
        if let Some(c) = &k.collation {
            b.push_attr("collate", self.collation_expr(c));
        }
        b
    }

    pub(super) fn index(&self, index: &Index, refs: bool) -> WBlock {
        let columns_expr = |names: &[String]| {
            if refs {
                columns_expr(names)
            } else {
                strings(names)
            }
        };
        let mut b = WBlock::new("index", vec![index.name.clone()]);
        let simple: Option<Vec<String>> = index
            .keys
            .iter()
            .map(|k| match (&k.expr, k) {
                (
                    KeyExpr::Column(c),
                    IndexKey {
                        opclass: None,
                        collation: None,
                        descending: false,
                        nulls: None,
                        ..
                    },
                ) => Some(c.clone()),
                _ => None,
            })
            .collect();
        match simple {
            Some(columns) if !columns.is_empty() => b.push_attr("columns", columns_expr(&columns)),
            _ => {
                for k in &index.keys {
                    b.push_block(self.index_key(k, refs));
                }
            }
        }
        if index.unique {
            b.push_attr("unique", WExpr::Bool(true));
        }
        if index.method != "btree" {
            b.push_attr("using", word(&index.method));
        }
        if !index.include.is_empty() {
            b.push_attr("include", columns_expr(&index.include));
        }
        if let Some(p) = &index.predicate {
            b.push_attr("where", WExpr::String(p.clone()));
        }
        if !index.nulls_distinct {
            b.push_attr("nulls_distinct", WExpr::Bool(false));
        }
        if !index.storage_params.is_empty() {
            b.push_attr("with", options(&index.storage_params));
        }
        if let Some(c) = &index.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        attributes_first(b)
    }

    fn policy(&self, p: &Policy) -> WBlock {
        let mut b = WBlock::new("policy", vec![p.name.clone()]);
        if p.command != PolicyCommand::All {
            b.push_attr("for", ident(p.command.as_sql()));
        }
        if !p.permissive {
            b.push_attr("as", ident("RESTRICTIVE"));
        }
        if p.roles != [PUBLIC] {
            b.push_attr(
                "to",
                WExpr::List(p.roles.iter().map(|r| self.role_expr(r)).collect()),
            );
        }
        if let Some(u) = &p.using {
            b.push_attr("using", WExpr::String(u.clone()));
        }
        if let Some(c) = &p.check {
            b.push_attr("check", WExpr::String(c.clone()));
        }
        if let Some(c) = &p.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    /// A trigger; `on` is set for top-level triggers.
    pub(super) fn trigger(&self, g: &Trigger, on: Option<(&str, WExpr)>) -> WBlock {
        let mut b = match on {
            Some((label, on)) => {
                let mut b = WBlock::new("trigger", vec![label.to_owned()]);
                if label != g.name {
                    b.push_attr("name", WExpr::String(g.name.clone()));
                }
                b.push_attr("on", on);
                b
            }
            None => WBlock::new("trigger", vec![g.name.clone()]),
        };
        let timing = match g.timing {
            TriggerTiming::Before => "before",
            TriggerTiming::After => "after",
            TriggerTiming::InsteadOf => "instead_of",
        };
        b.push_attr(
            timing,
            WExpr::List(g.events.iter().map(|e| ident(e.as_sql())).collect()),
        );
        if !g.update_of.is_empty() {
            b.push_attr("update_of", strings(&g.update_of));
        }
        b.push_attr(
            "for",
            ident(match g.level {
                TriggerLevel::Row => "ROW",
                TriggerLevel::Statement => "STATEMENT",
            }),
        );
        if let Some(w) = &g.when {
            b.push_attr("when", WExpr::String(w.clone()));
        }
        b.push_attr(
            "execute",
            self.function_expr(&g.function.schema, &g.function.name),
        );
        if !g.args.is_empty() {
            b.push_attr("args", strings(&g.args));
        }
        if let Some(c) = &g.constraint {
            b.push_attr("constraint", WExpr::Bool(true));
            push_deferrable(&mut b, c.deferrable, c.initially_deferred);
            if let Some(from) = &c.from {
                b.push_attr("from", self.table_expr(&from.schema, &from.name));
            }
        }
        if g.old_table.is_some() || g.new_table.is_some() {
            let mut entries = Vec::new();
            if let Some(t) = &g.old_table {
                entries.push(("old_table".to_owned(), WExpr::String(t.clone())));
            }
            if let Some(t) = &g.new_table {
                entries.push(("new_table".to_owned(), WExpr::String(t.clone())));
            }
            b.push_attr("referencing", WExpr::Object(entries));
        }
        if let Some(e) = enabled(g.enabled) {
            b.push_attr("enabled", e);
        }
        if let Some(c) = &g.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }
}

/// Moves nested blocks (index keys) after the attributes.
fn attributes_first(mut b: WBlock) -> WBlock {
    let (blocks, attrs): (Vec<WItem>, Vec<WItem>) = std::mem::take(&mut b.items)
        .into_iter()
        .partition(|i| matches!(i, WItem::Block(_)));
    b.items = attrs;
    b.items.extend(blocks);
    b
}

fn push_deferrable(b: &mut WBlock, deferrable: bool, initially_deferred: bool) {
    if deferrable {
        b.push_attr("deferrable", WExpr::Bool(true));
    }
    if initially_deferred {
        b.push_attr("initially_deferred", WExpr::Bool(true));
    }
}

fn push_validity(b: &mut WBlock, validated: bool, enforced: bool) {
    if !validated {
        b.push_attr("not_valid", WExpr::Bool(true));
    }
    if !enforced {
        b.push_attr("not_enforced", WExpr::Bool(true));
    }
}
