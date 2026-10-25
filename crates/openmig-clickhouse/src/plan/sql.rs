//! Rendering model objects as ClickHouse DDL.

use crate::diff::{ColumnChange, TableOp};
use crate::model::{
    Column, Constraint, DefaultKind, Dictionary, Function, Grant, Grantee, Index, InnerTable,
    MaterializedView, Projection, RoleGrant, RoleSet, RowPolicy, RowPolicyId, Table, View,
};
use openmig_core::Dialect;
use openmig_core::sql::{quote_ident, quote_literal};
use std::collections::BTreeMap;

pub fn q(name: &str) -> String {
    quote_ident(name, Dialect::ClickHouse)
}

pub fn lit(value: &str) -> String {
    quote_literal(value, Dialect::ClickHouse)
}

/// `` `database`.`name` ``.
pub fn qn(database: &str, name: &str) -> String {
    format!("{}.{}", q(database), q(name))
}

/// A key as written after `ORDER BY` / `PRIMARY KEY`: `tuple()` when empty, the element alone,
/// or a parenthesised list. Elements with a sort direction need `tuple(...)`.
pub fn key(elements: &[String]) -> String {
    let directed = elements
        .iter()
        .any(|e| e.ends_with(" DESC") || e.ends_with(" ASC"));
    match elements {
        [] => "tuple()".to_owned(),
        [one] if !directed => one.clone(),
        _ if directed => format!("tuple({})", elements.join(", ")),
        _ => format!("({})", elements.join(", ")),
    }
}

fn settings_list(settings: &BTreeMap<String, String>) -> String {
    settings
        .iter()
        .map(|(k, v)| format!("{} = {v}", q(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A column definition as in `CREATE TABLE` and `ADD COLUMN`.
pub fn column(c: &Column) -> String {
    let mut out = format!("{} {}", q(&c.name), c.data_type);
    if let Some(d) = &c.default {
        out.push(' ');
        out.push_str(d.kind.as_sql());
        if !d.expression.is_empty() {
            out.push(' ');
            out.push_str(&d.expression);
        }
    }
    if let Some(comment) = &c.comment {
        out.push_str(&format!(" COMMENT {}", lit(comment)));
    }
    if !c.codec.is_empty() {
        out.push_str(&format!(" CODEC({})", c.codec.join(", ")));
    }
    if !c.statistics.is_empty() {
        out.push_str(&format!(" STATISTICS({})", c.statistics.join(", ")));
    }
    if let Some(ttl) = &c.ttl {
        out.push_str(&format!(" TTL {ttl}"));
    }
    if !c.settings.is_empty() {
        out.push_str(&format!(" SETTINGS ({})", settings_list(&c.settings)));
    }
    out
}

pub fn index(name: &str, i: &Index) -> String {
    format!(
        "INDEX {} {} TYPE {} GRANULARITY {}",
        q(name),
        i.expression,
        i.index_type,
        i.granularity
    )
}

pub fn projection(name: &str, p: &Projection) -> String {
    let mut out = format!("PROJECTION {} ({})", q(name), p.query);
    if !p.settings.is_empty() {
        out.push_str(&format!(" WITH SETTINGS ({})", settings_list(&p.settings)));
    }
    out
}

pub fn constraint(name: &str, c: &Constraint) -> String {
    format!(
        "CONSTRAINT {} {} {}",
        q(name),
        c.kind.as_sql(),
        c.expression
    )
}

/// `ENGINE = ...` and the storage clauses, one per line.
fn storage(s: &InnerTable, merge_tree: bool, comment: Option<&str>) -> String {
    let mut out = format!("ENGINE = {}", s.engine);
    if let Some(p) = &s.partition_by {
        out.push_str(&format!("\nPARTITION BY {p}"));
    }
    if s.primary_key != s.order_by {
        out.push_str(&format!("\nPRIMARY KEY {}", key(&s.primary_key)));
    }
    if !s.order_by.is_empty() || merge_tree {
        out.push_str(&format!("\nORDER BY {}", key(&s.order_by)));
    }
    if let Some(sample) = &s.sample_by {
        out.push_str(&format!("\nSAMPLE BY {sample}"));
    }
    if let Some(ttl) = &s.ttl {
        out.push_str(&format!("\nTTL {ttl}"));
    }
    if !s.settings.is_empty() {
        out.push_str(&format!("\nSETTINGS {}", settings_list(&s.settings)));
    }
    if let Some(comment) = comment {
        out.push_str(&format!("\nCOMMENT {}", lit(comment)));
    }
    out
}

fn table_storage(t: &Table) -> InnerTable {
    InnerTable {
        engine: t.engine.clone(),
        partition_by: t.partition_by.clone(),
        order_by: t.order_by.clone(),
        primary_key: t.primary_key.clone(),
        sample_by: t.sample_by.clone(),
        ttl: t.ttl.clone(),
        settings: t.settings.clone(),
    }
}

pub fn create_table(database: &str, t: &Table, on_cluster: &str) -> String {
    let mut out = format!(
        "CREATE TABLE IF NOT EXISTS {}{on_cluster}",
        qn(database, &t.name)
    );
    let mut elements: Vec<String> = t.columns.iter().map(column).collect();
    elements.extend(t.indexes.iter().map(|(n, i)| index(n, i)));
    elements.extend(t.constraints.iter().map(|(n, c)| constraint(n, c)));
    elements.extend(t.projections.iter().map(|(n, p)| projection(n, p)));
    if !elements.is_empty() {
        out.push_str("\n(\n    ");
        out.push_str(&elements.join(",\n    "));
        out.push_str("\n)");
    }
    out.push('\n');
    if let Some(targets) = &t.time_series {
        let mut s = format!("ENGINE = {}", t.engine);
        if !t.settings.is_empty() {
            s.push_str(&format!("\nSETTINGS {}", settings_list(&t.settings)));
        }
        for (keyword, target) in [
            ("DATA", &targets.data),
            ("TAGS", &targets.tags),
            ("METRICS", &targets.metrics),
        ] {
            if let Some(target) = target {
                s.push_str(&format!("\n{keyword} {target}"));
            }
        }
        if let Some(comment) = &t.comment {
            s.push_str(&format!("\nCOMMENT {}", lit(comment)));
        }
        out.push_str(&s);
    } else {
        out.push_str(&storage(
            &table_storage(t),
            t.is_merge_tree(),
            t.comment.as_deref(),
        ));
    }
    out
}

/// One `ALTER TABLE` command (without `ALTER TABLE name`).
pub fn table_op(op: &TableOp) -> String {
    match op {
        TableOp::AddColumn { column: c, after } => {
            let position = match after {
                Some(after) => format!(" AFTER {}", q(after)),
                None => " FIRST".to_owned(),
            };
            format!("ADD COLUMN IF NOT EXISTS {}{position}", column(c))
        }
        TableOp::DropColumn(c) => format!("DROP COLUMN IF EXISTS {}", q(&c.name)),
        TableOp::RenameColumn { from, to } => {
            format!("RENAME COLUMN IF EXISTS {} TO {}", q(from), q(to))
        }
        TableOp::ModifyColumn { column, change } => {
            let c = q(column);
            match change {
                ColumnChange::Type(t) => format!("MODIFY COLUMN {c} {t}"),
                ColumnChange::Default(d) if d.expression.is_empty() => {
                    format!("MODIFY COLUMN {c} {}", d.kind.as_sql())
                }
                ColumnChange::Default(d) => {
                    format!("MODIFY COLUMN {c} {} {}", d.kind.as_sql(), d.expression)
                }
                ColumnChange::RemoveDefault(kind) => {
                    let kind = match kind {
                        DefaultKind::Ephemeral => "DEFAULT",
                        other => other.as_sql(),
                    };
                    format!("MODIFY COLUMN {c} REMOVE {kind}")
                }
                ColumnChange::Codec(codec) => {
                    format!("MODIFY COLUMN {c} CODEC({})", codec.join(", "))
                }
                ColumnChange::RemoveCodec => format!("MODIFY COLUMN {c} REMOVE CODEC"),
                ColumnChange::Ttl(ttl) => format!("MODIFY COLUMN {c} TTL {ttl}"),
                ColumnChange::RemoveTtl => format!("MODIFY COLUMN {c} REMOVE TTL"),
                ColumnChange::Comment(comment) => format!("COMMENT COLUMN {c} {}", lit(comment)),
                ColumnChange::RemoveComment => format!("MODIFY COLUMN {c} REMOVE COMMENT"),
            }
        }
        TableOp::AddIndex { name, index: i } => {
            format!("ADD {}", index(name, i)).replacen("INDEX", "INDEX IF NOT EXISTS", 1)
        }
        TableOp::DropIndex(name) => format!("DROP INDEX IF EXISTS {}", q(name)),
        TableOp::AddProjection {
            name,
            projection: p,
        } => format!("ADD {}", projection(name, p)).replacen(
            "PROJECTION",
            "PROJECTION IF NOT EXISTS",
            1,
        ),
        TableOp::DropProjection(name) => format!("DROP PROJECTION IF EXISTS {}", q(name)),
        TableOp::AddConstraint {
            name,
            constraint: c,
        } => format!("ADD {}", constraint(name, c)).replacen(
            "CONSTRAINT",
            "CONSTRAINT IF NOT EXISTS",
            1,
        ),
        TableOp::DropConstraint(name) => format!("DROP CONSTRAINT IF EXISTS {}", q(name)),
        TableOp::ModifyOrderBy(order_by) => format!("MODIFY ORDER BY {}", key(order_by)),
        TableOp::ModifySampleBy(sample) => format!("MODIFY SAMPLE BY {sample}"),
        TableOp::RemoveSampleBy => "REMOVE SAMPLE BY".to_owned(),
        TableOp::ModifyTtl(ttl) => format!("MODIFY TTL {ttl}"),
        TableOp::RemoveTtl => "REMOVE TTL".to_owned(),
        TableOp::ModifySettings(settings) => format!("MODIFY SETTING {}", settings_list(settings)),
        TableOp::ResetSettings(names) => format!(
            "RESET SETTING {}",
            names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", ")
        ),
        TableOp::ModifyComment(comment) => format!(
            "MODIFY COMMENT {}",
            lit(comment.as_deref().unwrap_or_default())
        ),
    }
}

fn security(sql_security: Option<&str>, definer: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(definer) = definer {
        out.push_str(&format!(" DEFINER = {}", q(definer)));
    }
    if let Some(security) = sql_security {
        out.push_str(&format!(" SQL SECURITY {security}"));
    }
    out
}

fn comment_clause(comment: Option<&str>) -> String {
    comment.map_or_else(String::new, |c| format!(" COMMENT {}", lit(c)))
}

pub fn create_view(database: &str, v: &View, on_cluster: &str) -> String {
    format!(
        "CREATE OR REPLACE VIEW {}{on_cluster}{}{} AS\n{}",
        qn(database, &v.name),
        security(v.sql_security.as_deref(), v.definer.as_deref()),
        comment_clause(v.comment.as_deref()),
        v.query
    )
}

pub fn create_materialized_view(database: &str, mv: &MaterializedView, on_cluster: &str) -> String {
    let mut out = format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {}{on_cluster}",
        qn(database, &mv.name)
    );
    if let Some(refresh) = &mv.refresh {
        out.push_str(&format!(" REFRESH {refresh}"));
    }
    if let Some(to) = &mv.to {
        out.push_str(&format!(" TO {}", qn(&to.database, &to.name)));
    }
    if let Some(inner) = &mv.inner {
        let merge_tree = crate::model::engine_name(&inner.engine).ends_with("MergeTree");
        out.push('\n');
        out.push_str(&storage(inner, merge_tree, None));
    }
    out.push_str(&security(mv.sql_security.as_deref(), mv.definer.as_deref()));
    out.push_str(&comment_clause(mv.comment.as_deref()));
    out.push_str(" AS\n");
    out.push_str(&mv.query);
    out
}

pub fn create_dictionary(database: &str, d: &Dictionary, on_cluster: &str) -> String {
    let attributes: Vec<String> = d
        .attributes
        .iter()
        .map(|a| {
            let mut out = format!("{} {}", q(&a.name), a.data_type);
            if let Some(default) = &a.default {
                out.push_str(&format!(" DEFAULT {default}"));
            }
            if let Some(expression) = &a.expression {
                out.push_str(&format!(" EXPRESSION {expression}"));
            }
            for (flag, word) in [
                (a.hierarchical, "HIERARCHICAL"),
                (a.bidirectional, "BIDIRECTIONAL"),
                (a.injective, "INJECTIVE"),
                (a.is_object_id, "IS_OBJECT_ID"),
            ] {
                if flag {
                    out.push(' ');
                    out.push_str(word);
                }
            }
            out
        })
        .collect();
    let mut out = format!(
        "CREATE OR REPLACE DICTIONARY {}{on_cluster}\n(\n    {}\n)\nPRIMARY KEY {}\nSOURCE({})",
        qn(database, &d.name),
        attributes.join(",\n    "),
        d.primary_key.join(", "),
        d.source
    );
    if let Some(lifetime) = &d.lifetime {
        out.push_str(&format!("\nLIFETIME({lifetime})"));
    }
    out.push_str(&format!("\nLAYOUT({})", d.layout));
    if let Some(range) = &d.range {
        out.push_str(&format!("\nRANGE({range})"));
    }
    if !d.settings.is_empty() {
        out.push_str(&format!("\nSETTINGS({})", settings_list(&d.settings)));
    }
    out.push_str(&comment_clause(d.comment.as_deref()).replacen(' ', "\n", 1));
    out
}

pub fn create_function(f: &Function, on_cluster: &str) -> String {
    let params: Vec<String> = f.parameters.iter().map(|p| q(p)).collect();
    format!(
        "CREATE OR REPLACE FUNCTION {}{on_cluster} AS ({}) -> {}",
        q(&f.name),
        params.join(", "),
        f.body
    )
}

fn grantee(g: &Grantee) -> String {
    q(g.name())
}

/// Privileges as written in `GRANT`/`REVOKE`, with the column list after each privilege.
fn privileges(g: &Grant) -> String {
    let columns = if g.columns.is_empty() {
        String::new()
    } else {
        format!(
            "({})",
            g.columns
                .iter()
                .map(|c| q(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    g.privileges
        .iter()
        .map(|p| format!("{p}{columns}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `GRANT` for a grant, `REVOKE` for a partial revoke.
pub fn grant(g: &Grant, on_cluster: &str) -> String {
    if g.partial_revoke {
        return revoke_statement(g, on_cluster, g.grant_option);
    }
    format!(
        "GRANT{on_cluster} {} ON {} TO {}{}",
        privileges(g),
        g.target_sql(),
        grantee(&g.grantee),
        if g.grant_option {
            " WITH GRANT OPTION"
        } else {
            ""
        }
    )
}

/// `REVOKE` for a grant, `GRANT` (lifting it) for a partial revoke.
pub fn revoke(g: &Grant, on_cluster: &str) -> String {
    if g.partial_revoke {
        return format!(
            "GRANT{on_cluster} {} ON {} TO {}",
            privileges(g),
            g.target_sql(),
            grantee(&g.grantee)
        );
    }
    revoke_statement(g, on_cluster, false)
}

/// `REVOKE GRANT OPTION FOR`.
pub fn revoke_grant_option(g: &Grant, on_cluster: &str) -> String {
    revoke_statement(g, on_cluster, true)
}

fn revoke_statement(g: &Grant, on_cluster: &str, grant_option_only: bool) -> String {
    format!(
        "REVOKE{on_cluster} {}{} ON {} FROM {}",
        if grant_option_only {
            "GRANT OPTION FOR "
        } else {
            ""
        },
        privileges(g),
        g.target_sql(),
        grantee(&g.grantee)
    )
}

pub fn grant_role(g: &RoleGrant, on_cluster: &str) -> String {
    format!(
        "GRANT{on_cluster} {} TO {}{}",
        q(&g.role),
        grantee(&g.grantee),
        if g.admin_option {
            " WITH ADMIN OPTION"
        } else {
            ""
        }
    )
}

pub fn revoke_role(g: &RoleGrant, on_cluster: &str) -> String {
    format!(
        "REVOKE{on_cluster} {} FROM {}",
        q(&g.role),
        grantee(&g.grantee)
    )
}

fn role_set(set: &RoleSet) -> String {
    let names = |names: &[String]| names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", ");
    if set.all {
        if set.except.is_empty() {
            "ALL".to_owned()
        } else {
            format!("ALL EXCEPT {}", names(&set.except))
        }
    } else if set.names.is_empty() {
        "NONE".to_owned()
    } else {
        names(&set.names)
    }
}

pub fn create_row_policy(id: &RowPolicyId, p: &RowPolicy, on_cluster: &str) -> String {
    let mut out = format!(
        "CREATE ROW POLICY OR REPLACE {}{on_cluster} ON {}",
        q(&id.name),
        qn(&id.database, &id.table)
    );
    if !p.storage.is_empty() {
        out.push_str(&format!(" IN {}", q(&p.storage)));
    }
    if let Some(filter) = &p.filter {
        out.push_str(&format!(" FOR SELECT USING {filter}"));
    }
    out.push_str(if p.restrictive {
        " AS RESTRICTIVE"
    } else {
        " AS PERMISSIVE"
    });
    out.push_str(&format!(" TO {}", role_set(&p.to)));
    out
}

pub fn drop_row_policy(id: &RowPolicyId, on_cluster: &str) -> String {
    format!(
        "DROP ROW POLICY IF EXISTS {} ON {}{on_cluster}",
        q(&id.name),
        qn(&id.database, &id.table)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnDefault, QualifiedName};
    use pretty_assertions::assert_eq;

    #[test]
    fn keys() {
        assert_eq!(key(&[]), "tuple()");
        assert_eq!(key(&["a".into()]), "a");
        assert_eq!(key(&["a".into(), "b".into()]), "(a, b)");
        assert_eq!(key(&["a".into(), "b DESC".into()]), "tuple(a, b DESC)");
    }

    #[test]
    fn tables() {
        let mut t = Table::new("events", "MergeTree");
        let mut ts = Column::new("ts", "DateTime64(3)");
        ts.codec = vec!["Delta(8)".into(), "ZSTD(1)".into()];
        let mut kind = Column::new("kind", "LowCardinality(String)");
        kind.default = Some(ColumnDefault {
            kind: DefaultKind::Default,
            expression: "'x'".into(),
        });
        kind.comment = Some("it's".into());
        t.columns = vec![ts, kind];
        t.order_by = vec!["ts".into()];
        t.primary_key = vec!["ts".into()];
        t.partition_by = Some("toYYYYMM(ts)".into());
        t.settings.insert("index_granularity".into(), "8192".into());
        t.indexes.insert(
            "k".into(),
            Index {
                expression: "kind".into(),
                index_type: "set(10)".into(),
                granularity: 1,
            },
        );
        assert_eq!(
            create_table("app", &t, ""),
            "CREATE TABLE IF NOT EXISTS `app`.`events`\n(\n    `ts` DateTime64(3) CODEC(Delta(8), ZSTD(1)),\n    `kind` LowCardinality(String) DEFAULT 'x' COMMENT 'it\\'s',\n    INDEX `k` kind TYPE set(10) GRANULARITY 1\n)\nENGINE = MergeTree\nPARTITION BY toYYYYMM(ts)\nORDER BY ts\nSETTINGS `index_granularity` = 8192"
        );
        let memory = Table::new("m", "Memory");
        assert_eq!(
            create_table("app", &memory, " ON CLUSTER `c`"),
            "CREATE TABLE IF NOT EXISTS `app`.`m` ON CLUSTER `c`\nENGINE = Memory"
        );
        let mut empty_key = Table::new("e", "MergeTree");
        empty_key.columns = vec![Column::new("a", "UInt8")];
        assert!(create_table("app", &empty_key, "").contains("ORDER BY tuple()"));
    }

    #[test]
    fn views_and_grants() {
        let mv = MaterializedView {
            name: "mv".into(),
            to: Some(QualifiedName::new("app", "dst")),
            query: "SELECT 1 AS x".into(),
            comment: Some("c".into()),
            ..MaterializedView::default()
        };
        assert_eq!(
            create_materialized_view("app", &mv, ""),
            "CREATE MATERIALIZED VIEW IF NOT EXISTS `app`.`mv` TO `app`.`dst` COMMENT 'c' AS\nSELECT 1 AS x"
        );
        let g = Grant {
            grantee: Grantee::Role("r".into()),
            database: Some("app".into()),
            table: Some("t".into()),
            columns: vec!["a".into(), "b".into()],
            privileges: ["INSERT".to_owned(), "SELECT".to_owned()].into(),
            grant_option: true,
            ..Grant::default()
        };
        assert_eq!(
            grant(&g, " ON CLUSTER `c`"),
            "GRANT ON CLUSTER `c` INSERT(`a`, `b`), SELECT(`a`, `b`) ON `app`.`t` TO `r` WITH GRANT OPTION"
        );
        assert_eq!(
            revoke(&g, ""),
            "REVOKE INSERT(`a`, `b`), SELECT(`a`, `b`) ON `app`.`t` FROM `r`"
        );
    }

    #[test]
    fn column_ops() {
        let op = TableOp::ModifyColumn {
            column: "s".into(),
            change: ColumnChange::RemoveDefault(DefaultKind::Materialized),
        };
        assert_eq!(table_op(&op), "MODIFY COLUMN `s` REMOVE MATERIALIZED");
        let add = TableOp::AddColumn {
            column: Column::new("c", "UInt8"),
            after: None,
        };
        assert_eq!(table_op(&add), "ADD COLUMN IF NOT EXISTS `c` UInt8 FIRST");
        let idx = TableOp::AddIndex {
            name: "i".into(),
            index: Index {
                expression: "a".into(),
                index_type: "minmax".into(),
                granularity: 2,
            },
        };
        assert_eq!(
            table_op(&idx),
            "ADD INDEX IF NOT EXISTS `i` a TYPE minmax GRANULARITY 2"
        );
        assert_eq!(table_op(&TableOp::ModifyComment(None)), "MODIFY COMMENT ''");
    }
}
