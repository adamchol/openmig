//! Encoding an inspected [`Model`] as HCL for `openmig inspect`.
//!
//! The output decodes back to the same model ([`crate::decode::decode`]) after normalization
//! through a scratch server: every value is written in a form the decoder turns into exactly the
//! inspected text, falling back to a plain string or `sql("...")` when no nicer form
//! round-trips. Attributes at their defaults are left out. Users, dependencies, materialized
//! view column lists (derived from the query) and the server version are never written; grants
//! that HCL cannot express (privileges on parameters, partial revokes) are left out with a warning.

mod values;

#[cfg(test)]
mod tests;

use crate::model::{
    Column, Database, DefaultKind, Dictionary, Function, Grant, GrantLevel, Grantee,
    MaterializedView, Model, Role, RowPolicy, RowPolicyId, Table, TimeSeriesTargets, View,
};
use crate::settings::ChSettings;
use openmig_core::{EncodeOptions, EncodedFile, SplitBy};
use openmig_hcl::Address;
use openmig_hcl::write::{WBlock, WExpr, WItem, render};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use values::{ident_or_string, literal_expr, sql_text_expr};

/// Target clauses the server gives a `TimeSeries` table that declares none (26.x).
const DEFAULT_TIME_SERIES: [(&str, &str); 3] = [
    ("data", "ENGINE = MergeTree ORDER BY (id, timestamp)"),
    (
        "tags",
        "ENGINE = AggregatingMergeTree PRIMARY KEY metric_name ORDER BY tuple(metric_name, id)",
    ),
    (
        "metrics",
        "ENGINE = ReplacingMergeTree ORDER BY metric_family_name",
    ),
];

/// Encode `model` as HCL files.
pub fn encode(model: &Model, settings: &ChSettings, options: &EncodeOptions) -> Vec<EncodedFile> {
    let encoder = Encoder { model, settings };
    let mut sections: Vec<(Section, WBlock)> = Vec::new();
    for role in model.roles.values() {
        sections.push((Section::cluster(Kind::Role), encoder.role(role)));
    }
    for function in model.functions.values() {
        sections.push((Section::cluster(Kind::Function), encoder.function(function)));
    }
    for db in model.databases.values() {
        if let Some(block) = encoder.database(db) {
            sections.push((Section::of(&db.name, Kind::Database), block));
        }
        for table in db.tables.values() {
            sections.push((Section::of(&db.name, Kind::Table), encoder.table(db, table)));
        }
        for dictionary in db.dictionaries.values() {
            sections.push((
                Section::of(&db.name, Kind::Dictionary),
                encoder.dictionary(db, dictionary),
            ));
        }
        for view in db.views.values() {
            sections.push((Section::of(&db.name, Kind::View), encoder.view(db, view)));
        }
        for mv in db.materialized_views.values() {
            sections.push((
                Section::of(&db.name, Kind::MaterializedView),
                encoder.materialized_view(db, mv),
            ));
        }
    }
    for (id, policy) in &model.row_policies {
        sections.push((
            Section::cluster(Kind::RowPolicy),
            encoder.row_policy(id, policy),
        ));
    }
    for grant in &model.grants {
        match encoder.grant(grant) {
            Some(block) => sections.push((Section::cluster(Kind::Grant), block)),
            None => tracing::warn!(
                "{} on {} for {} cannot be written in HCL and is left out",
                grant
                    .privileges
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                grant.target_sql(),
                grant.grantee
            ),
        }
    }
    files(sections, options.split)
}

/// Kinds of top-level blocks, in output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Role,
    Function,
    Database,
    Table,
    Dictionary,
    View,
    MaterializedView,
    RowPolicy,
    Grant,
}

impl Kind {
    fn file(self) -> &'static str {
        match self {
            Kind::Role => "roles.hcl",
            Kind::Function => "functions.hcl",
            Kind::Database => "databases.hcl",
            Kind::Table => "tables.hcl",
            Kind::Dictionary => "dictionaries.hcl",
            Kind::View => "views.hcl",
            Kind::MaterializedView => "materialized_views.hcl",
            Kind::RowPolicy => "row_policies.hcl",
            Kind::Grant => "grants.hcl",
        }
    }
}

/// Where a block goes: its database (`None` for server-wide objects) and kind.
struct Section {
    database: Option<String>,
    kind: Kind,
}

impl Section {
    fn cluster(kind: Kind) -> Self {
        Self {
            database: None,
            kind,
        }
    }

    fn of(database: &str, kind: Kind) -> Self {
        Self {
            database: Some(database.to_owned()),
            kind,
        }
    }
}

/// A file name for a database: path separators and NUL replaced.
fn database_file(database: &str) -> String {
    let safe: String = database
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
    let mut grouped: BTreeMap<String, Vec<WBlock>> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut database_files: BTreeMap<String, String> = BTreeMap::new();
    if split == SplitBy::Schema {
        used.insert("cluster.hcl".to_owned());
    }
    let mut sections = sections;
    if split == SplitBy::Kind {
        sections.sort_by_key(|(s, _)| s.kind);
    }
    for (section, block) in sections {
        let file = match split {
            SplitBy::None => "schema.hcl".to_owned(),
            SplitBy::Kind => section.kind.file().to_owned(),
            SplitBy::Schema => match &section.database {
                None => "cluster.hcl".to_owned(),
                Some(db) => database_files
                    .entry(db.clone())
                    .or_insert_with(|| {
                        let base = database_file(db);
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
        grouped.entry(file).or_default().push(block);
    }
    if grouped.is_empty() && split == SplitBy::None {
        grouped.insert("schema.hcl".to_owned(), Vec::new());
    }
    grouped
        .into_iter()
        .map(|(path, blocks)| EncodedFile {
            path: PathBuf::from(path),
            content: render(&blocks),
        })
        .collect()
}

struct Encoder<'a> {
    model: &'a Model,
    settings: &'a ChSettings,
}

fn string(s: &str) -> WExpr {
    WExpr::String(s.to_owned())
}

fn settings_expr(settings: &BTreeMap<String, String>) -> WExpr {
    WExpr::Object(
        settings
            .iter()
            .map(|(k, v)| (k.clone(), literal_expr(v)))
            .collect(),
    )
}

impl Encoder<'_> {
    /// The label and identity attributes of a block in `database`.
    fn qualified(&self, database: &str, name: &str) -> (String, Vec<WItem>) {
        if database.contains('.') {
            (
                name.to_owned(),
                vec![WItem::Attr("database".into(), string(database))],
            )
        } else {
            (format!("{database}.{name}"), Vec::new())
        }
    }

    /// The address a reference to `database.name` of `kind` uses.
    fn address(&self, kind: &str, database: &str, name: &str) -> Address {
        if database.contains('.') {
            Address::new(
                kind,
                vec![self.settings.default_database.clone(), name.to_owned()],
            )
        } else {
            Address::new(kind, vec![database.to_owned(), name.to_owned()])
        }
    }

    /// A reference to the table, view, materialized view or dictionary `database.name`, or the
    /// qualified name as a string when the model does not have it.
    fn relation_expr(&self, database: &str, name: &str) -> WExpr {
        let kind = self.model.databases.get(database).and_then(|db| {
            if db.tables.contains_key(name) {
                Some("table")
            } else if db.views.contains_key(name) {
                Some("view")
            } else if db.materialized_views.contains_key(name) {
                Some("materialized_view")
            } else if db.dictionaries.contains_key(name) {
                Some("dictionary")
            } else {
                None
            }
        });
        match kind {
            Some(kind) => WExpr::Ref(self.address(kind, database, name)),
            None => string(&format!("{database}.{name}")),
        }
    }

    fn role_expr(&self, name: &str) -> WExpr {
        if self.model.roles.contains_key(name) {
            WExpr::Ref(Address::new("role", vec![name.to_owned()]))
        } else {
            string(name)
        }
    }

    fn role(&self, role: &Role) -> WBlock {
        let mut block = WBlock::new("role", vec![role.name.clone()]);
        if !role.storage.is_empty() {
            block.push_attr("storage", ident_or_string(&role.storage));
        }
        let memberships: Vec<WExpr> = self
            .model
            .role_grants
            .iter()
            .filter(|g| g.grantee == Grantee::Role(role.name.clone()))
            .map(|g| {
                if g.admin_option {
                    WExpr::Object(vec![
                        ("role".into(), self.role_expr(&g.role)),
                        ("admin".into(), WExpr::Bool(true)),
                    ])
                } else {
                    self.role_expr(&g.role)
                }
            })
            .collect();
        if !memberships.is_empty() {
            block.push_attr("member_of", WExpr::List(memberships));
        }
        block
    }

    fn function(&self, function: &Function) -> WBlock {
        let mut block = WBlock::new("function", vec![function.name.clone()]);
        if !function.parameters.is_empty() {
            block.push_attr(
                "args",
                WExpr::List(function.parameters.iter().map(|p| string(p)).collect()),
            );
        }
        block.push_attr("body", string(&function.body));
        block
    }

    fn database(&self, db: &Database) -> Option<WBlock> {
        let implicit = db.name == "default" || db.name == self.settings.default_database;
        if implicit && db.engine == "Atomic" && db.comment.is_none() {
            return None;
        }
        let mut block = WBlock::new("database", vec![db.name.clone()]);
        if db.engine != "Atomic" {
            block.push_attr("engine", sql_text_expr(&db.engine));
        }
        if let Some(comment) = &db.comment {
            block.push_attr("comment", string(comment));
        }
        Some(block)
    }

    /// A key element: a (relative) reference when it is a column of the table, else the SQL
    /// text.
    fn key_expr(&self, table: &Table, element: &str) -> WExpr {
        let name = crate::ddl::unquote_ident(element);
        if table.column(&name).is_some() && crate::decode::values::column_expr(&name) == element {
            WExpr::Ref(Address::new("column", vec![name]))
        } else {
            string(element)
        }
    }

    fn table(&self, db: &Database, table: &Table) -> WBlock {
        let (label, mut items) = self.qualified(&db.name, &table.name);
        items.push(WItem::Attr("engine".into(), sql_text_expr(&table.engine)));
        let mut block = WBlock::new("table", vec![label]);
        block.items = items;
        for column in &table.columns {
            block.push_block(self.column(column));
        }
        if table.is_merge_tree() || !table.order_by.is_empty() {
            let order_by = if table.order_by.is_empty() {
                string("tuple()")
            } else {
                WExpr::List(
                    table
                        .order_by
                        .iter()
                        .map(|e| self.key_expr(table, e))
                        .collect(),
                )
            };
            block.push_attr("order_by", order_by);
        }
        if table.primary_key != table.order_by {
            block.push_attr(
                "primary_key",
                WExpr::List(
                    table
                        .primary_key
                        .iter()
                        .map(|e| self.key_expr(table, e))
                        .collect(),
                ),
            );
        }
        if let Some(p) = &table.partition_by {
            block.push_attr("partition_by", string(p));
        }
        if let Some(s) = &table.sample_by {
            block.push_attr("sample_by", string(s));
        }
        if let Some(ttl) = &table.ttl {
            block.push_attr("ttl", string(ttl));
        }
        if !table.settings.is_empty() {
            block.push_attr("settings", settings_expr(&table.settings));
        }
        if let Some(targets) = &table.time_series {
            self.time_series(targets, &mut block);
        }
        for (name, index) in &table.indexes {
            let mut b = WBlock::new("index", vec![name.clone()])
                .attr("expr", string(&index.expression))
                .attr("type", sql_text_expr(&index.index_type));
            if index.granularity != 1 {
                b.push_attr("granularity", WExpr::Number(index.granularity.to_string()));
            }
            block.push_block(b);
        }
        for (name, projection) in &table.projections {
            let mut b = WBlock::new("projection", vec![name.clone()])
                .attr("query", string(&projection.query));
            if !projection.settings.is_empty() {
                b.push_attr("settings", settings_expr(&projection.settings));
            }
            block.push_block(b);
        }
        for (name, constraint) in &table.constraints {
            let attr = match constraint.kind {
                crate::model::ConstraintKind::Check => "check",
                crate::model::ConstraintKind::Assume => "assume",
            };
            block.push_block(
                WBlock::new("constraint", vec![name.clone()])
                    .attr(attr, string(&constraint.expression)),
            );
        }
        if let Some(comment) = &table.comment {
            block.push_attr("comment", string(comment));
        }
        block
    }

    fn time_series(&self, targets: &TimeSeriesTargets, block: &mut WBlock) {
        for (attr, value) in [
            ("data", &targets.data),
            ("tags", &targets.tags),
            ("metrics", &targets.metrics),
        ] {
            let default = DEFAULT_TIME_SERIES
                .iter()
                .find(|(a, _)| *a == attr)
                .map(|(_, d)| *d);
            if let Some(value) = value
                && Some(value.as_str()) != default
            {
                block.push_attr(attr, string(value));
            }
        }
    }

    fn column(&self, column: &Column) -> WBlock {
        let mut block = WBlock::new("column", vec![column.name.clone()])
            .attr("type", sql_text_expr(&column.data_type));
        if let Some(d) = &column.default {
            let (attr, value) = match d.kind {
                DefaultKind::Default => ("default", literal_expr(&d.expression)),
                DefaultKind::Materialized => ("materialized", string(&d.expression)),
                DefaultKind::Alias => ("alias", string(&d.expression)),
                DefaultKind::Ephemeral => ("ephemeral", string(&d.expression)),
            };
            block.push_attr(attr, value);
        }
        if !column.codec.is_empty() {
            let items: Vec<WExpr> = column.codec.iter().map(|c| sql_text_expr(c)).collect();
            let value = if items.iter().all(|i| !matches!(i, WExpr::String(_))) {
                WExpr::List(items)
            } else {
                string(&column.codec.join(", "))
            };
            block.push_attr("codec", value);
        }
        if let Some(ttl) = &column.ttl {
            block.push_attr("ttl", string(ttl));
        }
        if !column.statistics.is_empty() {
            block.push_attr(
                "statistics",
                WExpr::List(column.statistics.iter().map(|s| string(s)).collect()),
            );
        }
        if !column.settings.is_empty() {
            block.push_attr("settings", settings_expr(&column.settings));
        }
        if let Some(comment) = &column.comment {
            block.push_attr("comment", string(comment));
        }
        block
    }

    fn security(&self, block: &mut WBlock, sql_security: Option<&str>, definer: Option<&str>) {
        if let Some(security) = sql_security {
            block.push_attr("sql_security", WExpr::Ident(security.to_owned()));
        }
        if let Some(definer) = definer {
            block.push_attr("definer", string(definer));
        }
    }

    fn view(&self, db: &Database, view: &View) -> WBlock {
        let (label, items) = self.qualified(&db.name, &view.name);
        let mut block = WBlock::new("view", vec![label]);
        block.items = items;
        block.push_attr("as", string(&view.query));
        self.security(
            &mut block,
            view.sql_security.as_deref(),
            view.definer.as_deref(),
        );
        if let Some(comment) = &view.comment {
            block.push_attr("comment", string(comment));
        }
        block
    }

    fn materialized_view(&self, db: &Database, mv: &MaterializedView) -> WBlock {
        let (label, items) = self.qualified(&db.name, &mv.name);
        let mut block = WBlock::new("materialized_view", vec![label]);
        block.items = items;
        if let Some(to) = &mv.to {
            let target = match self.relation_expr(&to.database, &to.name) {
                WExpr::Ref(a) if a.kind == "table" => WExpr::Ref(a),
                _ => string(&format!("{}.{}", to.database, to.name)),
            };
            block.push_attr("to", target);
        }
        if let Some(refresh) = &mv.refresh {
            block.push_attr("refresh", string(refresh));
        }
        if let Some(inner) = &mv.inner {
            block.push_attr("engine", sql_text_expr(&inner.engine));
            let key = |key: &[String]| WExpr::List(key.iter().map(|e| string(e)).collect());
            if !inner.order_by.is_empty() {
                block.push_attr("order_by", key(&inner.order_by));
            } else if crate::model::engine_name(&inner.engine).ends_with("MergeTree") {
                block.push_attr("order_by", string("tuple()"));
            }
            if inner.primary_key != inner.order_by {
                block.push_attr("primary_key", key(&inner.primary_key));
            }
            if let Some(p) = &inner.partition_by {
                block.push_attr("partition_by", string(p));
            }
            if let Some(s) = &inner.sample_by {
                block.push_attr("sample_by", string(s));
            }
            if let Some(ttl) = &inner.ttl {
                block.push_attr("ttl", string(ttl));
            }
            if !inner.settings.is_empty() {
                block.push_attr("settings", settings_expr(&inner.settings));
            }
        }
        block.push_attr("as", string(&mv.query));
        self.security(
            &mut block,
            mv.sql_security.as_deref(),
            mv.definer.as_deref(),
        );
        if let Some(comment) = &mv.comment {
            block.push_attr("comment", string(comment));
        }
        block
    }

    fn dictionary(&self, db: &Database, dictionary: &Dictionary) -> WBlock {
        let (label, items) = self.qualified(&db.name, &dictionary.name);
        let mut block = WBlock::new("dictionary", vec![label]);
        block.items = items;
        for a in &dictionary.attributes {
            let mut b = WBlock::new("attribute", vec![a.name.clone()])
                .attr("type", sql_text_expr(&a.data_type));
            if let Some(default) = &a.default {
                b.push_attr("default", literal_expr(default));
            }
            if let Some(expression) = &a.expression {
                b.push_attr("expression", string(expression));
            }
            for (flag, attr) in [
                (a.hierarchical, "hierarchical"),
                (a.bidirectional, "bidirectional"),
                (a.injective, "injective"),
                (a.is_object_id, "is_object_id"),
            ] {
                if flag {
                    b.push_attr(attr, WExpr::Bool(true));
                }
            }
            block.push_block(b);
        }
        block.push_attr(
            "primary_key",
            WExpr::List(dictionary.primary_key.iter().map(|k| string(k)).collect()),
        );
        block.push_attr("source", string(&dictionary.source));
        block.push_attr("layout", sql_text_expr(&dictionary.layout));
        if let Some(lifetime) = &dictionary.lifetime {
            block.push_attr("lifetime", string(lifetime));
        }
        if let Some(range) = &dictionary.range {
            block.push_attr("range", string(range));
        }
        if !dictionary.settings.is_empty() {
            block.push_attr("settings", settings_expr(&dictionary.settings));
        }
        if let Some(comment) = &dictionary.comment {
            block.push_attr("comment", string(comment));
        }
        block
    }

    fn row_policy(&self, id: &RowPolicyId, policy: &RowPolicy) -> WBlock {
        let mut block = WBlock::new("row_policy", vec![id.name.clone()])
            .attr("on", self.relation_expr(&id.database, &id.table));
        if let Some(filter) = &policy.filter {
            block.push_attr("using", string(filter));
        }
        if policy.restrictive {
            block.push_attr("as", WExpr::Ident("RESTRICTIVE".into()));
        }
        let mut to: Vec<WExpr> = Vec::new();
        if policy.to.all {
            to.push(WExpr::Ident("ALL".into()));
        }
        to.extend(policy.to.names.iter().map(|r| self.role_expr(r)));
        block.push_attr("to", WExpr::List(to));
        if !policy.to.except.is_empty() {
            block.push_attr(
                "except",
                WExpr::List(policy.to.except.iter().map(|r| self.role_expr(r)).collect()),
            );
        }
        block
    }

    /// A grant block; `None` for partial revokes and privileges on parameters, which HCL cannot
    /// express.
    fn grant(&self, grant: &Grant) -> Option<WBlock> {
        if grant.partial_revoke || grant.level() == GrantLevel::Parameter {
            return None;
        }
        let mut block = WBlock::new("grant", Vec::new());
        let privileges: Vec<WExpr> = grant
            .privileges
            .iter()
            .map(|p| {
                if p.chars().all(|c| c.is_ascii_alphanumeric()) && values::is_hcl_ident(p) {
                    WExpr::Ident(p.clone())
                } else {
                    string(p)
                }
            })
            .collect();
        block.push_attr("privileges", WExpr::List(privileges));
        let on = match (&grant.database, &grant.table) {
            (None, _) => string("*.*"),
            (Some(d), None) => {
                if self.model.databases.contains_key(d) || d == "default" {
                    WExpr::Ref(Address::new("database", vec![d.clone()]))
                } else {
                    string(&format!("{d}.*"))
                }
            }
            (Some(d), Some(t)) => self.relation_expr(d, t),
        };
        block.push_attr("on", WExpr::List(vec![on]));
        if !grant.columns.is_empty() {
            block.push_attr(
                "columns",
                WExpr::List(grant.columns.iter().map(|c| string(c)).collect()),
            );
        }
        block.push_attr(
            "to",
            WExpr::List(vec![self.role_expr(grant.grantee.name())]),
        );
        if grant.grant_option {
            block.push_attr("with_grant_option", WExpr::Bool(true));
        }
        Some(block)
    }
}
