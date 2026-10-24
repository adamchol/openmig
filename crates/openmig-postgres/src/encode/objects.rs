//! Schemas, cluster-wide objects, types, sequences, views, routines and data.

use super::Encoder;
use super::values::{enabled, ident, number, options, scalar, strings, word};
use crate::decode::objects::view_query;
use crate::decode::routine::{default_cost, default_rows, implied_return};
use crate::decode::types::TypeContext;
use crate::model::{
    ArgMode, Composite, Domain, Enum, EventTrigger, Extension, Function, MaterializedView,
    Membership, Parallel, QualifiedName, Returns, Role, Schema, Sequence, SequenceOptions,
    TableData, View, ViewCheckOption, ViewColumn, Volatility,
};
use openmig_hcl::write::{WBlock, WExpr};

/// A view query as a heredoc-friendly string: trimmed, ending with a newline.
fn query_text(query: &str) -> String {
    format!("{}\n", view_query(query))
}

impl Encoder<'_> {
    pub(super) fn schema_block(&self, schema: &Schema) -> WBlock {
        let mut b = WBlock::new("schema", vec![schema.name.clone()]);
        let (default_comment, _) = crate::decode::schema_defaults(self.settings, &schema.name);
        if schema.comment != default_comment {
            b.push_attr(
                "comment",
                WExpr::String(schema.comment.clone().unwrap_or_default()),
            );
        }
        b
    }

    pub(super) fn role(&self, role: &Role) -> WBlock {
        let mut b = WBlock::new("role", vec![role.name.clone()]);
        let defaults = Role::new(&role.name);
        for (name, value, default) in [
            ("login", role.login, defaults.login),
            ("superuser", role.superuser, defaults.superuser),
            ("createdb", role.createdb, defaults.createdb),
            ("createrole", role.createrole, defaults.createrole),
            ("inherit", role.inherit, defaults.inherit),
            ("replication", role.replication, defaults.replication),
            ("bypassrls", role.bypassrls, defaults.bypassrls),
        ] {
            if value != default {
                b.push_attr(name, WExpr::Bool(value));
            }
        }
        if role.connection_limit != -1 {
            b.push_attr("connection_limit", number(role.connection_limit));
        }
        if let Some(v) = &role.valid_until {
            b.push_attr("valid_until", WExpr::String(v.clone()));
        }
        if !role.member_of.is_empty() {
            let default = Membership {
                admin: false,
                inherit: role.inherit,
                set: true,
            };
            let items = role
                .member_of
                .iter()
                .map(|(target, m)| {
                    let target = self.role_expr(target);
                    if *m == default {
                        return target;
                    }
                    let mut entries = vec![("role".to_owned(), target)];
                    if m.admin != default.admin {
                        entries.push(("admin".into(), WExpr::Bool(m.admin)));
                    }
                    if m.inherit != default.inherit {
                        entries.push(("inherit".into(), WExpr::Bool(m.inherit)));
                    }
                    if m.set != default.set {
                        entries.push(("set".into(), WExpr::Bool(m.set)));
                    }
                    WExpr::Object(entries)
                })
                .collect();
            b.push_attr("member_of", WExpr::List(items));
        }
        if let Some(c) = &role.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    pub(super) fn extension(&self, e: &Extension) -> WBlock {
        let mut b = WBlock::new("extension", vec![e.name.clone()]);
        if e.schema != self.settings.default_schema {
            b.push_attr("schema", self.schema_expr(&e.schema));
        }
        if !e.version.is_empty() {
            b.push_attr("version", WExpr::String(e.version.clone()));
        }
        if let Some(c) = &e.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    pub(super) fn enum_block(&self, schema: &Schema, e: &Enum) -> WBlock {
        let mut b = self.qualified_block("enum", &schema.name, &e.name);
        b.push_attr("values", strings(&e.values));
        if let Some(c) = &e.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    pub(super) fn composite(&self, schema: &Schema, c: &Composite) -> WBlock {
        let mut b = self.qualified_block("composite", &schema.name, &c.name);
        if let Some(comment) = &c.comment {
            b.push_attr("comment", WExpr::String(comment.clone()));
        }
        for f in &c.fields {
            let mut field = WBlock::new("field", vec![f.name.clone()]);
            field.push_attr("type", self.type_expr(&f.data_type, TypeContext::Column));
            if let Some(collation) = &f.collation {
                field.push_attr("collate", self.collation_expr(collation));
            }
            b.push_block(field);
        }
        b
    }

    pub(super) fn domain(&self, schema: &Schema, d: &Domain) -> WBlock {
        let mut b = self.qualified_block("domain", &schema.name, &d.name);
        b.push_attr("type", self.type_expr(&d.data_type, TypeContext::Column));
        if d.not_null {
            b.push_attr("null", WExpr::Bool(false));
        }
        if let Some(default) = &d.default {
            b.push_attr("default", self.default_expr(default, &d.data_type));
        }
        if let Some(collation) = &d.collation {
            b.push_attr("collate", self.collation_expr(collation));
        }
        if let Some(c) = &d.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        for check in d.checks.values() {
            let mut k = WBlock::new("check", vec![check.name.clone()]);
            k.push_attr("expr", WExpr::String(check.expr.clone()));
            if !check.validated {
                k.push_attr("not_valid", WExpr::Bool(true));
            }
            if let Some(c) = &check.comment {
                k.push_attr("comment", WExpr::String(c.clone()));
            }
            b.push_block(k);
        }
        b
    }

    pub(super) fn sequence(&self, schema: &Schema, q: &Sequence) -> WBlock {
        let mut b = self.qualified_block("sequence", &schema.name, &q.name);
        if q.data_type != "bigint" {
            b.push_attr("type", self.type_expr(&q.data_type, TypeContext::Column));
        }
        let o = &q.options;
        let increment = (o.increment != 1).then_some(o.increment);
        let base = crate::decode::table::sequence_options(
            &q.data_type,
            &crate::decode::table::SequenceParams {
                increment,
                ..Default::default()
            },
        );
        let min = (o.min_value != base.min_value).then_some(o.min_value);
        let max = (o.max_value != base.max_value).then_some(o.max_value);
        let derived: SequenceOptions = crate::decode::table::sequence_options(
            &q.data_type,
            &crate::decode::table::SequenceParams {
                increment,
                min,
                max,
                ..Default::default()
            },
        );
        if o.start != derived.start {
            b.push_attr("start", number(o.start));
        }
        if let Some(i) = increment {
            b.push_attr("increment", number(i));
        }
        if let Some(m) = min {
            b.push_attr("min", number(m));
        }
        if let Some(m) = max {
            b.push_attr("max", number(m));
        }
        if o.cache != 1 {
            b.push_attr("cache", number(o.cache));
        }
        if o.cycle {
            b.push_attr("cycle", WExpr::Bool(true));
        }
        if let Some(owner) = &q.owned_by
            && let Some(column) = self.column_expr(&owner.schema, &owner.table, &owner.column)
        {
            b.push_attr("owned_by", column);
        }
        if let Some(c) = &q.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    /// `column` blocks for view columns with a comment or privileges.
    fn view_columns(&self, b: &mut WBlock, columns: &[ViewColumn]) {
        for c in columns {
            if c.comment.is_none() && c.acl.is_empty() {
                continue;
            }
            let mut column = WBlock::new("column", vec![c.name.clone()]);
            if let Some(comment) = &c.comment {
                column.push_attr("comment", WExpr::String(comment.clone()));
            }
            b.push_block(column);
        }
    }

    pub(super) fn view(&self, schema: &Schema, v: &View) -> WBlock {
        let mut b = self.qualified_block("view", &schema.name, &v.name);
        if v.security_invoker {
            b.push_attr("security_invoker", WExpr::Bool(true));
        }
        if v.security_barrier {
            b.push_attr("security_barrier", WExpr::Bool(true));
        }
        match v.check_option {
            Some(ViewCheckOption::Local) => b.push_attr("check_option", ident("LOCAL")),
            Some(ViewCheckOption::Cascaded) => b.push_attr("check_option", ident("CASCADED")),
            None => {}
        }
        if let Some(c) = &v.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b.push_attr("as", WExpr::String(query_text(&v.query)));
        self.view_columns(&mut b, &v.columns);
        b
    }

    pub(super) fn materialized_view(&self, schema: &Schema, m: &MaterializedView) -> WBlock {
        let mut b = self.qualified_block("materialized_view", &schema.name, &m.name);
        if !m.with_data {
            b.push_attr("with_data", WExpr::Bool(false));
        }
        if !m.storage_params.is_empty() {
            b.push_attr("storage_params", options(&m.storage_params));
        }
        if let Some(c) = &m.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b.push_attr("as", WExpr::String(query_text(&m.query)));
        self.view_columns(&mut b, &m.columns);
        for index in m.indexes.values() {
            b.push_block(self.index(index, false));
        }
        b
    }

    pub(super) fn routine(
        &self,
        schema: &Schema,
        signature: &str,
        f: &Function,
        procedure: bool,
    ) -> WBlock {
        let s = schema.name.as_str();
        let labels = if procedure {
            &self.procedure_labels
        } else {
            &self.function_labels
        };
        let label = labels
            .get(&(s.to_owned(), signature.to_owned()))
            .cloned()
            .unwrap_or_else(|| f.name.clone());
        let mut b =
            self.qualified_block(if procedure { "procedure" } else { "function" }, s, &label);
        if label != f.name {
            b.push_attr("name", WExpr::String(f.name.clone()));
        }
        for arg in &f.args {
            let mut a = WBlock::new("arg", arg.name.iter().cloned().collect());
            a.push_attr("type", self.type_expr(&arg.data_type, TypeContext::Routine));
            if arg.mode != ArgMode::In {
                a.push_attr("mode", ident(arg.mode.as_sql()));
            }
            if let Some(d) = &arg.default {
                a.push_attr("default", self.default_expr(d, &arg.data_type));
            }
            b.push_block(a);
        }
        match &f.returns {
            None => {}
            Some(Returns::Table(columns)) => {
                let mut t = WBlock::new("returns_table", Vec::new());
                for c in columns {
                    let mut column = WBlock::new("column", vec![c.name.clone()]);
                    column.push_attr("type", self.type_expr(&c.data_type, TypeContext::Routine));
                    t.push_block(column);
                }
                b.push_block(t);
            }
            Some(Returns::Type { data_type, setof }) => {
                let implied = !setof && implied_return(&f.args).as_deref() == Some(data_type);
                if !implied {
                    b.push_attr("returns", self.type_expr(data_type, TypeContext::Routine));
                }
                if *setof {
                    b.push_attr("setof", WExpr::Bool(true));
                }
            }
        }
        b.push_attr("language", word(&f.language));
        if f.volatility != Volatility::Volatile {
            b.push_attr("volatility", ident(f.volatility.as_sql()));
        }
        if f.strict {
            b.push_attr("strict", WExpr::Bool(true));
        }
        if f.security_definer {
            b.push_attr("security", ident("DEFINER"));
        }
        if f.leakproof {
            b.push_attr("leakproof", WExpr::Bool(true));
        }
        if f.parallel != Parallel::Unsafe {
            b.push_attr("parallel", ident(f.parallel.as_sql()));
        }
        if !procedure {
            if f.cost != default_cost(&f.language) {
                b.push_attr("cost", scalar(&f.cost));
            }
            if f.rows != default_rows(f.returns.as_ref()) {
                b.push_attr("rows", scalar(&f.rows));
            }
            if f.window {
                b.push_attr("window", WExpr::Bool(true));
            }
        }
        if !f.config.is_empty() {
            b.push_attr(
                "set",
                WExpr::Object(
                    f.config
                        .iter()
                        .map(|(k, v)| (k.clone(), WExpr::String(v.clone())))
                        .collect(),
                ),
            );
        }
        if let Some(c) = &f.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b.push_attr("body", WExpr::String(f.body.text().to_owned()));
        b
    }

    /// Top-level `trigger` blocks for the triggers of the views of `schema`. Labels are trigger
    /// names, prefixed with the view name when a name is used more than once in the model.
    pub(super) fn view_triggers(&self, schema: &Schema) -> Vec<WBlock> {
        let mut count: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for s in self.model.schemas.values() {
            for v in s.views.values() {
                for name in v.triggers.keys() {
                    *count.entry(name.as_str()).or_default() += 1;
                }
            }
        }
        let mut out = Vec::new();
        for v in schema.views.values() {
            for g in v.triggers.values() {
                let label = if count[g.name.as_str()] == 1 {
                    g.name.clone()
                } else {
                    format!("{}.{}.{}", schema.name, v.name, g.name)
                };
                let on = WExpr::Ref(self.address("view", &schema.name, &v.name));
                out.push(self.trigger(g, Some((&label, on))));
            }
        }
        out
    }

    pub(super) fn event_trigger(&self, e: &EventTrigger) -> WBlock {
        let mut b = WBlock::new("event_trigger", vec![e.name.clone()]);
        b.push_attr("on", word(&e.event));
        if !e.tags.is_empty() {
            b.push_attr("tags", strings(&e.tags));
        }
        b.push_attr(
            "execute",
            self.function_expr(&e.function.schema, &e.function.name),
        );
        if let Some(value) = enabled(e.enabled) {
            b.push_attr("enabled", value);
        }
        if let Some(c) = &e.comment {
            b.push_attr("comment", WExpr::String(c.clone()));
        }
        b
    }

    pub(super) fn data(&self, table: &QualifiedName, data: &TableData) -> WBlock {
        let mut b = self.qualified_block("data", &table.schema, &table.name);
        let t = self.model.table(table);
        let pk = t.and_then(|t| t.primary_key.as_ref()).map(|pk| &pk.columns);
        if pk != Some(&data.key) {
            b.push_attr("key", strings(&data.key));
        }
        let order: Vec<&str> = t
            .map(|t| t.columns.iter().map(|c| c.name.as_str()).collect())
            .unwrap_or_default();
        let position = |name: &str| order.iter().position(|c| *c == name).unwrap_or(usize::MAX);
        let rows = data
            .rows
            .values()
            .map(|row| {
                let mut entries: Vec<(&String, &Option<String>)> = row.iter().collect();
                entries.sort_by_key(|(name, _)| (position(name), (*name).clone()));
                WExpr::Object(
                    entries
                        .into_iter()
                        .map(|(name, value)| {
                            let v = match value {
                                None => WExpr::Null,
                                Some(text) => scalar(text),
                            };
                            (name.clone(), v)
                        })
                        .collect(),
                )
            })
            .collect();
        b.push_attr("rows", WExpr::List(rows));
        if let Some(mode) = data.mode {
            let m = match mode {
                openmig_core::DataSync::Sync => "sync",
                openmig_core::DataSync::Insert => "insert",
            };
            b.push_attr("mode", ident(m));
        }
        b
    }
}
