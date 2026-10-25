//! Altering a table in place.

use super::{ColumnChange, TableOp};
use crate::ddl;
use crate::model::{Column, Table};
use openmig_core::Dialect;
use openmig_core::sql::{TokenKind, tokenize};
use std::collections::{BTreeMap, BTreeSet};

/// `index_granularity` is fixed when a table is created; the server adds it to every MergeTree
/// table's settings.
const FIXED_SETTINGS: &[&str] = &["index_granularity"];

/// The `ALTER TABLE` commands that turn `old` into `new`, in execution order, or why the table
/// cannot be altered into `new`.
pub fn diff_table(old: &Table, new: &Table) -> Result<Vec<TableOp>, String> {
    if old.engine != new.engine {
        return Err(format!(
            "the engine changes from {} to {}",
            old.engine, new.engine
        ));
    }
    if old.time_series != new.time_series {
        return Err("the TimeSeries target tables change".to_owned());
    }
    if old.partition_by != new.partition_by {
        return Err(format!(
            "PARTITION BY changes from {} to {}",
            old.partition_by.as_deref().unwrap_or("nothing"),
            new.partition_by.as_deref().unwrap_or("nothing")
        ));
    }
    let added: Vec<&Column> = new
        .columns
        .iter()
        .filter(|c| old.column(&c.name).is_none())
        .collect();
    let mut order_by = None;
    if old.order_by != new.order_by {
        let appended = new.order_by.starts_with(&old.order_by)
            && new.order_by[old.order_by.len()..].iter().all(|e| {
                let name = ddl::unquote_ident(e);
                added.iter().any(|c| c.name == name)
            });
        if !appended {
            return Err(format!(
                "ORDER BY changes from {} to {}; only appending newly added columns is supported",
                key_text(&old.order_by),
                key_text(&new.order_by)
            ));
        }
        if old.primary_key != new.primary_key {
            return Err(format!(
                "appending columns to ORDER BY keeps the primary key {}: declare \
                 `primary_key` with it",
                key_text(&old.primary_key)
            ));
        }
        order_by = Some(new.order_by.clone());
    }
    if old.primary_key != new.primary_key {
        return Err(format!(
            "the primary key changes from {} to {}",
            key_text(&old.primary_key),
            key_text(&new.primary_key)
        ));
    }
    for name in FIXED_SETTINGS {
        if let (Some(a), Some(b)) = (old.settings.get(*name), new.settings.get(*name))
            && a != b
        {
            return Err(format!("setting {name} changes from {a} to {b}"));
        }
    }

    let keys = key_columns(old);
    let mut early = Vec::new();
    let mut columns = Vec::new();
    let mut late = Vec::new();

    for (name, _) in old
        .indexes
        .iter()
        .filter(|(n, i)| new.indexes.get(*n) != Some(*i))
    {
        early.push(TableOp::DropIndex(name.clone()));
    }
    for (name, _) in old
        .projections
        .iter()
        .filter(|(n, p)| new.projections.get(*n) != Some(*p))
    {
        early.push(TableOp::DropProjection(name.clone()));
    }
    for (name, _) in old
        .constraints
        .iter()
        .filter(|(n, c)| new.constraints.get(*n) != Some(*c))
    {
        early.push(TableOp::DropConstraint(name.clone()));
    }
    if old.ttl.is_some() && new.ttl.is_none() {
        early.push(TableOp::RemoveTtl);
    }
    if old.sample_by.is_some() && new.sample_by.is_none() {
        early.push(TableOp::RemoveSampleBy);
    }

    for column in &old.columns {
        if new.column(&column.name).is_none() {
            columns.push(TableOp::DropColumn(column.clone()));
        }
    }
    for column in &new.columns {
        if let Some(have) = old.column(&column.name) {
            columns.extend(column_ops(have, column, keys.contains(&column.name))?);
        }
    }
    for (i, column) in new.columns.iter().enumerate() {
        if old.column(&column.name).is_none() {
            columns.push(TableOp::AddColumn {
                column: column.clone(),
                after: i.checked_sub(1).map(|p| new.columns[p].name.clone()),
            });
        }
    }
    if let Some(order_by) = order_by {
        columns.push(TableOp::ModifyOrderBy(order_by));
    }

    for (name, index) in &new.indexes {
        if old.indexes.get(name) != Some(index) {
            late.push(TableOp::AddIndex {
                name: name.clone(),
                index: index.clone(),
            });
        }
    }
    for (name, projection) in &new.projections {
        if old.projections.get(name) != Some(projection) {
            late.push(TableOp::AddProjection {
                name: name.clone(),
                projection: projection.clone(),
            });
        }
    }
    for (name, constraint) in &new.constraints {
        if old.constraints.get(name) != Some(constraint) {
            late.push(TableOp::AddConstraint {
                name: name.clone(),
                constraint: constraint.clone(),
            });
        }
    }
    if let Some(sample_by) = &new.sample_by
        && old.sample_by.as_ref() != Some(sample_by)
    {
        late.push(TableOp::ModifySampleBy(sample_by.clone()));
    }
    if let Some(ttl) = &new.ttl
        && old.ttl.as_ref() != Some(ttl)
    {
        late.push(TableOp::ModifyTtl(ttl.clone()));
    }
    let modified: BTreeMap<String, String> = new
        .settings
        .iter()
        .filter(|(k, v)| old.settings.get(*k) != Some(*v) && !FIXED_SETTINGS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !modified.is_empty() {
        late.push(TableOp::ModifySettings(modified));
    }
    let reset: Vec<String> = old
        .settings
        .keys()
        .filter(|k| !new.settings.contains_key(*k) && !FIXED_SETTINGS.contains(&k.as_str()))
        .cloned()
        .collect();
    if !reset.is_empty() {
        late.push(TableOp::ResetSettings(reset));
    }
    if old.comment != new.comment {
        late.push(TableOp::ModifyComment(new.comment.clone()));
    }
    Ok(early.into_iter().chain(columns).chain(late).collect())
}

/// `MODIFY COLUMN` commands for one column: removals first (a codec must go before a column
/// becomes an `ALIAS`), then the type, the default, the codec, the TTL and the comment.
fn column_ops(old: &Column, new: &Column, key: bool) -> Result<Vec<TableOp>, String> {
    if old.statistics != new.statistics {
        return Err(format!(
            "the statistics of column {} change; change them by hand",
            new.name
        ));
    }
    if old.settings != new.settings {
        return Err(format!(
            "the settings of column {} change; change them by hand",
            new.name
        ));
    }
    let mut changes = Vec::new();
    if old.codec != new.codec && new.codec.is_empty() {
        changes.push(ColumnChange::RemoveCodec);
    }
    if old.ttl.is_some() && new.ttl.is_none() {
        changes.push(ColumnChange::RemoveTtl);
    }
    if old.comment.is_some() && new.comment.is_none() {
        changes.push(ColumnChange::RemoveComment);
    }
    if let (Some(d), None) = (&old.default, &new.default) {
        changes.push(ColumnChange::RemoveDefault(d.kind));
    }
    if old.data_type != new.data_type {
        if key {
            return Err(format!(
                "the type of key column {} changes from {} to {}",
                new.name, old.data_type, new.data_type
            ));
        }
        changes.push(ColumnChange::Type(new.data_type.clone()));
    }
    if let Some(d) = &new.default
        && old.default.as_ref() != Some(d)
    {
        changes.push(ColumnChange::Default(d.clone()));
    }
    if old.codec != new.codec && !new.codec.is_empty() {
        changes.push(ColumnChange::Codec(new.codec.clone()));
    }
    if let Some(ttl) = &new.ttl
        && old.ttl.as_ref() != Some(ttl)
    {
        changes.push(ColumnChange::Ttl(ttl.clone()));
    }
    if let Some(comment) = &new.comment
        && old.comment.as_ref() != Some(comment)
    {
        changes.push(ColumnChange::Comment(comment.clone()));
    }
    Ok(changes
        .into_iter()
        .map(|change| TableOp::ModifyColumn {
            column: new.name.clone(),
            change,
        })
        .collect())
}

/// Columns named in the sorting, primary, partition or sampling key.
fn key_columns(table: &Table) -> BTreeSet<String> {
    let names: BTreeSet<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    let mut out = BTreeSet::new();
    let exprs = table
        .order_by
        .iter()
        .chain(&table.primary_key)
        .map(String::as_str)
        .chain(table.partition_by.as_deref())
        .chain(table.sample_by.as_deref());
    for expr in exprs {
        for token in tokenize(expr, Dialect::ClickHouse).unwrap_or_default() {
            let name = match token.kind {
                TokenKind::Word => token.text.to_owned(),
                TokenKind::QuotedIdent => ddl::unquote_ident(token.text),
                _ => continue,
            };
            if names.contains(name.as_str()) {
                out.insert(name);
            }
        }
    }
    out
}

fn key_text(key: &[String]) -> String {
    if key.is_empty() {
        "tuple()".to_owned()
    } else {
        format!("({})", key.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnDefault, DefaultKind, Index};
    use pretty_assertions::assert_eq;

    fn table(columns: &[(&str, &str)]) -> Table {
        let mut t = Table::new("t", "MergeTree");
        t.columns = columns.iter().map(|(n, ty)| Column::new(*n, *ty)).collect();
        t.order_by = vec!["a".into()];
        t.primary_key = vec!["a".into()];
        t.settings.insert("index_granularity".into(), "8192".into());
        t
    }

    fn summaries(ops: &[TableOp]) -> Vec<String> {
        ops.iter().map(TableOp::summary).collect()
    }

    #[test]
    fn columns_are_added_dropped_and_modified() {
        let old = table(&[("a", "UInt32"), ("b", "String"), ("legacy", "String")]);
        let mut new = table(&[
            ("a", "UInt32"),
            ("b", "LowCardinality(String)"),
            ("c", "UInt8"),
        ]);
        new.columns[1].comment = Some("kind".into());
        new.columns[2].default = Some(ColumnDefault {
            kind: DefaultKind::Default,
            expression: "0".into(),
        });
        let ops = diff_table(&old, &new).unwrap();
        assert_eq!(
            summaries(&ops),
            [
                "drop column legacy",
                "column b type LowCardinality(String)",
                "column b comment",
                "add column c",
            ]
        );
        let TableOp::AddColumn { after, .. } = &ops[3] else {
            panic!()
        };
        assert_eq!(after.as_deref(), Some("b"));
    }

    #[test]
    fn removals_come_first() {
        let mut old = table(&[("a", "UInt32"), ("s", "String")]);
        old.columns[1].codec = vec!["ZSTD(1)".into()];
        old.columns[1].default = Some(ColumnDefault {
            kind: DefaultKind::Materialized,
            expression: "'x'".into(),
        });
        let mut new = table(&[("a", "UInt32"), ("s", "String")]);
        new.columns[1].default = Some(ColumnDefault {
            kind: DefaultKind::Alias,
            expression: "toString(a)".into(),
        });
        let ops = diff_table(&old, &new).unwrap();
        assert_eq!(summaries(&ops), ["column s drop codec", "column s ALIAS"]);
    }

    #[test]
    fn keys_and_engines() {
        let old = table(&[("a", "UInt32"), ("b", "UInt32")]);
        let mut new = old.clone();
        new.order_by = vec!["b".into()];
        new.primary_key = vec!["b".into()];
        assert!(
            diff_table(&old, &new)
                .unwrap_err()
                .contains("ORDER BY changes")
        );

        let mut key_type = old.clone();
        key_type.columns[0].data_type = "UInt64".into();
        assert!(
            diff_table(&old, &key_type)
                .unwrap_err()
                .contains("key column a")
        );

        let mut engine = old.clone();
        engine.engine = "ReplacingMergeTree".into();
        assert!(diff_table(&old, &engine).unwrap_err().contains("engine"));

        let mut granularity = old.clone();
        granularity
            .settings
            .insert("index_granularity".into(), "1024".into());
        assert!(
            diff_table(&old, &granularity)
                .unwrap_err()
                .contains("index_granularity")
        );

        let mut appended = table(&[("a", "UInt32"), ("b", "UInt32"), ("c", "UInt32")]);
        appended.order_by = vec!["a".into(), "c".into()];
        let ops = diff_table(&old, &appended).unwrap();
        assert_eq!(summaries(&ops), ["add column c", "order by"]);
        appended.primary_key = appended.order_by.clone();
        assert!(
            diff_table(&old, &appended)
                .unwrap_err()
                .contains("keeps the primary key")
        );
    }

    #[test]
    fn indexes_ttl_settings_comment() {
        let mut old = table(&[("a", "UInt32")]);
        old.indexes.insert(
            "i".into(),
            Index {
                expression: "a".into(),
                index_type: "minmax".into(),
                granularity: 1,
            },
        );
        old.ttl = Some("x".into());
        old.settings
            .insert("ttl_only_drop_parts".into(), "1".into());
        let mut new = table(&[("a", "UInt32")]);
        new.indexes.insert(
            "i".into(),
            Index {
                expression: "a".into(),
                index_type: "set(10)".into(),
                granularity: 1,
            },
        );
        new.settings
            .insert("merge_with_ttl_timeout".into(), "100".into());
        new.comment = Some("c".into());
        new.settings.remove("index_granularity");
        let ops = diff_table(&old, &new).unwrap();
        assert_eq!(
            summaries(&ops),
            [
                "drop index i",
                "drop TTL",
                "add index i",
                "settings merge_with_ttl_timeout",
                "reset settings ttl_only_drop_parts",
                "comment",
            ]
        );
        assert!(diff_table(&old, &old).unwrap().is_empty());
    }
}
