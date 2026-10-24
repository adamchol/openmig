//! `data` blocks: declared rows of a table.

use super::Decoder;
use super::keywords::DataMode;
use super::types::scalar_text;
use crate::model::{QualifiedName, TableData};
use openmig_core::{DataSync, ObjectKind, ObjectRef};
use openmig_hcl::decode::suggest;
use openmig_hcl::{Block, Diagnostic, Value};
use std::collections::BTreeMap;

impl<'a> Decoder<'a> {
    pub(super) fn data(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let span = block.header_span;
        let mode = dec.keyword::<DataMode>("mode").map(|m| m.0);
        let key_attr = dec.attr("key").filter(|a| !matches!(a.value, Value::Null));
        let rows_attr = dec.attr("rows").filter(|a| !matches!(a.value, Value::Null));
        dec.finish();
        let Some(table) = self.db.table(&qn) else {
            self.error(
                span,
                format!("`data` for `{qn}`, which is not declared as a table"),
            );
            return;
        };
        let columns: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let pk = table.primary_key.as_ref().map(|pk| pk.columns.clone());
        let key = match key_attr {
            Some(attr) => {
                let mut names = Vec::new();
                match &attr.value {
                    Value::List(items) => {
                        for item in items {
                            match item {
                                Value::String(s) => names.push(s.clone()),
                                Value::Ref(a) => match self.column_ref(a) {
                                    Some((t, c)) if t == qn => names.push(c),
                                    _ => self.error(
                                        attr.value_span,
                                        format!("`{a}` is not a column of `{qn}`"),
                                    ),
                                },
                                other => self.error(
                                    attr.value_span,
                                    format!(
                                        "`key` entries must be column names, found {}",
                                        other.type_name()
                                    ),
                                ),
                            }
                        }
                    }
                    other => self.error(
                        attr.value_span,
                        format!(
                            "`key` must be a list of column names, found {}",
                            other.type_name()
                        ),
                    ),
                }
                for name in &names {
                    if !columns.contains(name) {
                        self.error(
                            attr.value_span,
                            format!("unknown column `{name}` in `{qn}`"),
                        );
                    }
                }
                names
            }
            None => match pk {
                Some(pk) => pk,
                None => {
                    self.error(
                        span,
                        format!("`data` for `{qn}` needs `key`: the table has no primary key"),
                    );
                    return;
                }
            },
        };
        if key.is_empty() {
            self.error(span, "`key` cannot be empty");
            return;
        }
        let mut rows = BTreeMap::new();
        if let Some(attr) = rows_attr {
            let Value::List(items) = &attr.value else {
                self.error(
                    attr.value_span,
                    format!(
                        "`rows` must be a list of objects, found {}",
                        attr.value.type_name()
                    ),
                );
                return;
            };
            for (i, item) in items.iter().enumerate() {
                let n = i + 1;
                let Value::Object(map) = item else {
                    self.error(
                        attr.value_span,
                        format!("row {n} must be an object, found {}", item.type_name()),
                    );
                    continue;
                };
                let mut row = BTreeMap::new();
                let mut ok = true;
                for (column, value) in map {
                    if !columns.contains(column) {
                        let mut d = Diagnostic::error(format!(
                            "row {n}: unknown column `{column}` in `{qn}`"
                        ))
                        .at(attr.value_span);
                        if let Some(s) = suggest(column, columns.iter().map(String::as_str)) {
                            d = d.with_help(format!("did you mean `{s}`?"));
                        }
                        self.push(d);
                        ok = false;
                        continue;
                    }
                    let text = match value {
                        Value::Sql(_) => Err(format!(
                            "row {n}, `{column}`: sql() is not supported in data rows; write the \
                             value"
                        )),
                        Value::Ident(_) => Err(format!(
                            "row {n}, `{column}`: identifiers are not values; quote the text"
                        )),
                        other => {
                            scalar_text(other).map_err(|m| format!("row {n}, `{column}`: {m}"))
                        }
                    };
                    match text {
                        Ok(text) => {
                            row.insert(column.clone(), text);
                        }
                        Err(message) => {
                            self.error(attr.value_span, message);
                            ok = false;
                        }
                    }
                }
                let missing: Vec<&String> = key.iter().filter(|k| !row.contains_key(*k)).collect();
                if !missing.is_empty() {
                    self.error(
                        attr.value_span,
                        format!(
                            "row {n} of `{qn}` has no value for the key column(s) {}",
                            missing
                                .iter()
                                .map(|m| format!("`{m}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                    continue;
                }
                if !ok {
                    continue;
                }
                let row_key: Vec<Option<String>> = key.iter().map(|k| row[k].clone()).collect();
                if rows.insert(row_key.clone(), row).is_some() {
                    self.duplicate_row(attr.value_span, &qn, &key, &row_key, Some(n));
                }
            }
        }
        self.merge_data(qn, key, rows, mode, span);
    }

    fn duplicate_row(
        &self,
        span: openmig_hcl::Span,
        table: &QualifiedName,
        key: &[String],
        values: &[Option<String>],
        row: Option<usize>,
    ) {
        let shown: Vec<String> = key
            .iter()
            .zip(values)
            .map(|(k, v)| format!("{k} = {}", v.as_deref().unwrap_or("null")))
            .collect();
        let at = row.map(|n| format!("row {n}: ")).unwrap_or_default();
        self.error(
            span,
            format!("{at}duplicate row in `{table}`: {}", shown.join(", ")),
        );
    }

    /// Adds rows to the table's data, merging with other `data` blocks of the same table.
    fn merge_data(
        &mut self,
        table: QualifiedName,
        key: Vec<String>,
        rows: BTreeMap<Vec<Option<String>>, BTreeMap<String, Option<String>>>,
        mode: Option<DataSync>,
        span: openmig_hcl::Span,
    ) {
        let r = ObjectRef::new(ObjectKind::Data, [&table.schema, &table.name]);
        let first = self.origins.get(&r).copied();
        let Some(existing) = self.db.data.get_mut(&table) else {
            self.origins.insert(r, span);
            self.db.data.insert(table, TableData { key, rows, mode });
            return;
        };
        let mut problems = Vec::new();
        if existing.key != key {
            problems.push(format!(
                "`data` blocks for `{table}` use different keys ({} and {})",
                existing.key.join(", "),
                key.join(", ")
            ));
        }
        if mode.is_some() && existing.mode.is_some() && existing.mode != mode {
            problems.push(format!("`data` blocks for `{table}` use different modes"));
        }
        existing.mode = existing.mode.or(mode);
        let mut duplicates = Vec::new();
        for (k, row) in rows {
            if existing.rows.insert(k.clone(), row).is_some() {
                duplicates.push(k);
            }
        }
        let key = existing.key.clone();
        for message in problems {
            let mut d = Diagnostic::error(message).at(span);
            if let Some(first) = first {
                d = d.with_related(first, "first `data` block");
            }
            self.push(d);
        }
        for k in duplicates {
            self.duplicate_row(span, &table, &key, &k, None);
        }
    }
}
