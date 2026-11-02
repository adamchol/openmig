//! `publication` blocks.

use super::Decoder;
use super::keywords::PublishOp;
use crate::model::{Publication, PublishedTable, QualifiedName};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::{Block, Value};
use std::collections::btree_map::Entry;

impl<'a> Decoder<'a> {
    /// Decoded after tables, which a publication's `table` blocks name by label.
    pub(super) fn publication(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.nested_name(&mut dec) else {
            return;
        };
        let r = ObjectRef::new(ObjectKind::Publication, [&name]);
        let mut p = Publication::new(&name);
        for table in self.table_refs(&mut dec, "tables") {
            p.tables.insert(table, PublishedTable::default());
        }
        for mut child in dec.children("table") {
            let label = child.block().labels.first().cloned().unwrap_or_default();
            let table = match label.split_once('.') {
                Some((schema, table)) => QualifiedName::new(schema, table),
                None => QualifiedName::new(self.settings.default_schema.as_str(), label.as_str()),
            };
            let columns = child.opt::<Vec<String>>("columns").unwrap_or_default();
            let filter = child.opt::<String>("where").filter(|w| !w.is_empty());
            let span = child.block().header_span;
            child.finish();
            if self.db.table(&table).is_none() {
                self.error(span, format!("table `{table}` is not declared"));
                continue;
            }
            match p.tables.entry(table) {
                Entry::Occupied(e) => self.error(
                    span,
                    format!("table `{}` is published more than once", e.key()),
                ),
                Entry::Vacant(e) => {
                    e.insert(PublishedTable { columns, filter });
                }
            }
        }
        p.schemas = self.schema_names(&mut dec, "schemas");
        if let Some(ops) = self.keyword_list::<PublishOp>(&mut dec, "publish") {
            p.insert = ops.contains(&PublishOp::Insert);
            p.update = ops.contains(&PublishOp::Update);
            p.delete = ops.contains(&PublishOp::Delete);
            p.truncate = ops.contains(&PublishOp::Truncate);
        }
        p.via_partition_root = dec
            .opt::<bool>("publish_via_partition_root")
            .unwrap_or(false);
        p.comment = self.comment(&mut dec);
        self.depends_on(&mut dec, &r);
        dec.finish();
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        self.db.publications.insert(name, p);
    }

    /// A list of schema references or names, sorted.
    fn schema_names(&self, dec: &mut openmig_hcl::BlockDecoder<'_>, attr: &str) -> Vec<String> {
        let Some(a) = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null)) else {
            return Vec::new();
        };
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!("`{attr}` must be a list of schema references"),
            );
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::Ref(r) if r.kind == "schema" => {
                    if let Some(s) = self.symbol(r) {
                        out.push(s.name.clone());
                    }
                }
                Value::String(s) if !s.is_empty() => out.push(s.clone()),
                other => self.error(
                    a.value_span,
                    format!(
                        "`{attr}` entries must be schema references, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
