//! Databases, tables, views, materialized views, dictionaries and functions.

use super::Decoder;
use super::values::{codec, column_expr, expression, literal, sql_expr, sql_text};
use crate::ddl;
use crate::model::{
    Column, ColumnDefault, Constraint, ConstraintKind, DefaultKind, Dictionary,
    DictionaryAttribute, Function, Index, InnerTable, MaterializedView, Projection, QualifiedName,
    Table, View, engine_name,
};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::decode::suggest;
use openmig_hcl::{Address, Block, BlockDecoder, Diagnostic, Number, Value};

const SQL_SECURITY: &[&str] = &["DEFINER", "INVOKER", "NONE"];

impl<'a> Decoder<'a> {
    pub(super) fn database(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.block_name(&mut dec) else {
            dec.finish();
            return;
        };
        let r = ObjectRef::new(ObjectKind::Database, [name.as_str()]);
        if crate::settings::SYSTEM_DATABASES.contains(&name.as_str()) {
            self.error(block.header_span, format!("`{name}` is a system database"));
        }
        if !self.declare(r, block.header_span) {
            dec.finish();
            return;
        }
        let engine = self
            .sql_attr(&mut dec, "engine", false, sql_text)
            .unwrap_or_else(|| "Atomic".to_owned());
        let comment = self.comment(&mut dec);
        dec.finish();
        let db = self.database_mut(&name);
        db.engine = engine;
        db.comment = comment;
    }

    pub(super) fn table(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let (database, name) = self.qualified_name(&mut dec);
        if !self.declare_relation(ObjectKind::Table, &database, &name, block.header_span) {
            dec.finish();
            return;
        }
        let table_ref = ObjectRef::new(ObjectKind::Table, [&database, &name]);
        let mut table = Table::new(&name, "");
        table.engine = self
            .sql_attr(&mut dec, "engine", true, sql_text)
            .unwrap_or_default();
        let time_series = engine_name(&table.engine) == "TimeSeries";

        for mut child in dec.children("column") {
            if let Some(column) = self.column(&mut child, &database, &name) {
                if table.column(&column.name).is_some() {
                    self.error(
                        child.block().header_span,
                        format!("column `{}` is declared more than once", column.name),
                    );
                } else {
                    table.columns.push(column);
                }
            }
            child.finish();
        }
        if time_series && !table.columns.is_empty() {
            self.error(
                block.header_span,
                "`TimeSeries` tables declare no columns: they are implicit",
            );
        }
        let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let address = block.address.clone();
        table.order_by = self
            .key_attr(&mut dec, "order_by", address.as_ref(), &names)
            .unwrap_or_default();
        table.primary_key = self
            .key_attr(&mut dec, "primary_key", address.as_ref(), &names)
            .unwrap_or_else(|| table.order_by.clone());
        table.partition_by = self.sql_attr(&mut dec, "partition_by", false, expression);
        table.sample_by = self.sql_attr(&mut dec, "sample_by", false, expression);
        table.ttl = self.sql_attr(&mut dec, "ttl", false, expression);
        table.settings = self.settings_attr(&mut dec, "settings");
        if time_series {
            table.time_series = self.sql_attr(&mut dec, "targets", false, expression);
        }
        for mut child in dec.children("index") {
            if let Some((index_name, index)) = self.index(&mut child, &database, &name) {
                table.indexes.insert(index_name, index);
            }
            child.finish();
        }
        for mut child in dec.children("projection") {
            if let Some((projection_name, projection)) =
                self.projection(&mut child, &database, &name)
            {
                table.projections.insert(projection_name, projection);
            }
            child.finish();
        }
        for mut child in dec.children("constraint") {
            if let Some((constraint_name, constraint)) =
                self.constraint(&mut child, &database, &name)
            {
                table.constraints.insert(constraint_name, constraint);
            }
            child.finish();
        }
        table.comment = self.comment(&mut dec);
        self.renamed_from(&mut dec, ObjectKind::Table, &[&database, &name]);
        self.depends_on(&mut dec, &table_ref);
        dec.finish();
        self.database_mut(&database).tables.insert(name, table);
    }

    fn column(
        &mut self,
        dec: &mut BlockDecoder<'a>,
        database: &str,
        table: &str,
    ) -> Option<Column> {
        let name = self.block_name(dec)?;
        let span = dec.block().header_span;
        let r = ObjectRef::new(ObjectKind::Column, [database, table, name.as_str()]);
        if !self.declare(r, span) {
            return None;
        }
        let mut column = Column::new(&name, "");
        column.data_type = self.sql_attr(dec, "type", true, sql_text)?;
        let kinds = [
            ("default", DefaultKind::Default),
            ("materialized", DefaultKind::Materialized),
            ("alias", DefaultKind::Alias),
            ("ephemeral", DefaultKind::Ephemeral),
        ];
        let mut found: Vec<&str> = Vec::new();
        for (attr, kind) in kinds {
            let Some(a) = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null)) else {
                continue;
            };
            found.push(attr);
            let text = match kind {
                DefaultKind::Default => literal(&a.value),
                _ => expression(&a.value),
            };
            match text {
                Ok(text) if text.is_empty() && kind != DefaultKind::Ephemeral => {
                    self.error(a.value_span, format!("`{attr}` cannot be empty"));
                }
                Ok(expression) => column.default = Some(ColumnDefault { kind, expression }),
                Err(expected) => self.error(a.value_span, format!("`{attr}` must be {expected}")),
            }
        }
        if found.len() > 1 {
            self.error(
                span,
                format!(
                    "a column takes at most one of `default`, `materialized`, `alias` and \
                     `ephemeral`, found {}",
                    found.join(", ")
                ),
            );
        }
        if let Some(a) = dec
            .attr("codec")
            .filter(|a| !matches!(a.value, Value::Null))
        {
            match codec(&a.value) {
                Ok(list) => column.codec = list,
                Err(expected) => self.error(a.value_span, format!("`codec` must be {expected}")),
            }
        }
        column.ttl = self.sql_attr(dec, "ttl", false, expression);
        column.statistics = self.string_list(dec, "statistics").unwrap_or_default();
        column.settings = self.settings_attr(dec, "settings");
        column.comment = self.comment(dec);
        self.renamed_from(dec, ObjectKind::Column, &[database, table, &name]);
        Some(column)
    }

    /// `order_by` / `primary_key`: a list of column references and SQL expressions, or one SQL
    /// expression (`"tuple()"`, `"(a, b)"`).
    fn key_attr(
        &self,
        dec: &mut BlockDecoder<'_>,
        name: &str,
        table: Option<&Address>,
        columns: &[String],
    ) -> Option<Vec<String>> {
        let attr = dec.attr(name).filter(|a| !matches!(a.value, Value::Null))?;
        let items = match &attr.value {
            Value::List(items) => items,
            Value::String(s) | Value::Sql(s) => {
                return match ddl::split_key(s) {
                    Ok(key) => Some(key),
                    Err(e) => {
                        self.error(attr.value_span, format!("`{name}`: {}", e.message));
                        None
                    }
                };
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`{name}` must be a list of column references and expressions, or a \
                         string, found {}",
                        other.type_name()
                    ),
                );
                return None;
            }
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::Ref(a) => match self.own_column(a, table, columns) {
                    Ok(column) => out.push(column_expr(&column)),
                    Err(message) => self.push(self.column_error(attr.value_span, message, columns)),
                },
                Value::String(s) | Value::Sql(s) if !s.trim().is_empty() => {
                    out.push(s.trim().to_owned());
                }
                Value::Call { .. } | Value::Ident(_) => match sql_expr(item) {
                    Ok(text) => out.push(text),
                    Err(message) => self.error(attr.value_span, message),
                },
                other => self.error(
                    attr.value_span,
                    format!(
                        "`{name}` entries must be column references or SQL strings, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        Some(out)
    }

    /// The name of a column of the enclosing table that `address` refers to.
    fn own_column(
        &self,
        address: &Address,
        table: Option<&Address>,
        columns: &[String],
    ) -> Result<String, String> {
        let is_column =
            address.kind == "table" && address.nested.len() == 1 && address.nested[0].0 == "column";
        if !is_column {
            return Err(format!("`{address}` is not a column"));
        }
        if table.is_some_and(|t| t != &address.root()) {
            return Err(format!("`{address}` is a column of another table"));
        }
        let name = self
            .nested_db_name(address)
            .ok_or_else(|| format!("unknown column `{address}`"))?;
        if !columns.contains(&name) {
            return Err(format!("unknown column `{name}`"));
        }
        Ok(name)
    }

    fn column_error(
        &self,
        span: openmig_hcl::Span,
        message: String,
        columns: &[String],
    ) -> Diagnostic {
        let mut d = Diagnostic::error(message.clone()).at(span);
        if let Some(name) = message
            .strip_prefix("unknown column `")
            .and_then(|m| m.strip_suffix('`'))
            && let Some(s) = suggest(name, columns.iter().map(String::as_str))
        {
            d = d.with_help(format!("did you mean `{s}`?"));
        }
        d
    }

    fn index(
        &mut self,
        dec: &mut BlockDecoder<'a>,
        database: &str,
        table: &str,
    ) -> Option<(String, Index)> {
        let name = self.block_name(dec)?;
        let r = ObjectRef::new(ObjectKind::Index, [database, table, name.as_str()]);
        if !self.declare(r, dec.block().header_span) {
            return None;
        }
        let expression = self.sql_attr(dec, "expr", true, expression);
        let index_type = self.sql_attr(dec, "type", true, sql_text);
        let granularity = match dec.opt::<i64>("granularity") {
            Some(g) if g >= 1 => g as u64,
            Some(_) => {
                self.error(dec.block().header_span, "`granularity` must be at least 1");
                1
            }
            None => 1,
        };
        Some((
            name,
            Index {
                expression: expression?,
                index_type: index_type?,
                granularity,
            },
        ))
    }

    fn projection(
        &mut self,
        dec: &mut BlockDecoder<'a>,
        database: &str,
        table: &str,
    ) -> Option<(String, Projection)> {
        let name = self.block_name(dec)?;
        let r = ObjectRef::new(ObjectKind::Projection, [database, table, name.as_str()]);
        if !self.declare(r, dec.block().header_span) {
            return None;
        }
        let query = self.sql_attr(dec, "query", true, expression)?;
        let settings = self.settings_attr(dec, "settings");
        Some((name, Projection { query, settings }))
    }

    fn constraint(
        &mut self,
        dec: &mut BlockDecoder<'a>,
        database: &str,
        table: &str,
    ) -> Option<(String, Constraint)> {
        let name = self.block_name(dec)?;
        let span = dec.block().header_span;
        let r = ObjectRef::new(ObjectKind::Constraint, [database, table, name.as_str()]);
        if !self.declare(r, span) {
            return None;
        }
        let check = self.sql_attr(dec, "check", false, expression);
        let assume = self.sql_attr(dec, "assume", false, expression);
        match (check, assume) {
            (Some(expression), None) => Some((
                name,
                Constraint {
                    kind: ConstraintKind::Check,
                    expression,
                },
            )),
            (None, Some(expression)) => Some((
                name,
                Constraint {
                    kind: ConstraintKind::Assume,
                    expression,
                },
            )),
            _ => {
                self.error(
                    span,
                    "a constraint takes exactly one of `check` and `assume`",
                );
                None
            }
        }
    }

    /// `sql_security` and `definer` of views.
    fn security(&self, dec: &mut BlockDecoder<'_>) -> (Option<String>, Option<String>) {
        let security = dec
            .attr("sql_security")
            .filter(|a| !matches!(a.value, Value::Null))
            .and_then(|a| match &a.value {
                Value::Ident(w) | Value::String(w)
                    if SQL_SECURITY.contains(&w.to_uppercase().as_str()) =>
                {
                    Some(w.to_uppercase())
                }
                _ => {
                    self.error(
                        a.value_span,
                        "`sql_security` must be one of DEFINER, INVOKER or NONE",
                    );
                    None
                }
            });
        let definer = dec.opt::<String>("definer").filter(|d| !d.is_empty());
        (security, definer)
    }

    pub(super) fn view(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let (database, name) = self.qualified_name(&mut dec);
        if !self.declare_relation(ObjectKind::View, &database, &name, block.header_span) {
            dec.finish();
            return;
        }
        let r = ObjectRef::new(ObjectKind::View, [&database, &name]);
        let query = self.sql_attr(&mut dec, "as", true, expression);
        let (sql_security, definer) = self.security(&mut dec);
        let comment = self.comment(&mut dec);
        self.depends_on(&mut dec, &r);
        dec.finish();
        let view = View {
            name: name.clone(),
            query: query.unwrap_or_default(),
            sql_security,
            definer,
            comment,
        };
        self.database_mut(&database).views.insert(name, view);
    }

    pub(super) fn materialized_view(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let (database, name) = self.qualified_name(&mut dec);
        if !self.declare_relation(
            ObjectKind::MaterializedView,
            &database,
            &name,
            block.header_span,
        ) {
            dec.finish();
            return;
        }
        let r = ObjectRef::new(ObjectKind::MaterializedView, [&database, &name]);
        let to = self.target_table(&mut dec, &database);
        let query = self.sql_attr(&mut dec, "as", true, expression);
        let refresh = self.sql_attr(&mut dec, "refresh", false, expression);
        let inner = self
            .sql_attr(&mut dec, "engine", false, sql_text)
            .map(|engine| {
                let order_by = self
                    .key_attr(&mut dec, "order_by", None, &[])
                    .unwrap_or_default();
                InnerTable {
                    engine,
                    primary_key: self
                        .key_attr(&mut dec, "primary_key", None, &[])
                        .unwrap_or_else(|| order_by.clone()),
                    order_by,
                    partition_by: self.sql_attr(&mut dec, "partition_by", false, expression),
                    sample_by: self.sql_attr(&mut dec, "sample_by", false, expression),
                    ttl: self.sql_attr(&mut dec, "ttl", false, expression),
                    settings: self.settings_attr(&mut dec, "settings"),
                }
            });
        match (&to, &inner) {
            (Some(_), Some(_)) => self.error(
                block.header_span,
                "a materialized view takes either `to` or `engine`, not both",
            ),
            (None, None) => self.error(
                block.header_span,
                "a materialized view needs `to` (target table) or `engine` (inner table)",
            ),
            _ => {}
        }
        let (sql_security, definer) = self.security(&mut dec);
        let comment = self.comment(&mut dec);
        self.depends_on(&mut dec, &r);
        dec.finish();
        let mv = MaterializedView {
            name: name.clone(),
            to,
            query: query.unwrap_or_default(),
            columns: Vec::new(),
            refresh,
            inner,
            sql_security,
            definer,
            comment,
        };
        self.database_mut(&database)
            .materialized_views
            .insert(name, mv);
    }

    /// The `to` table of a materialized view: a table reference or `"database.table"`.
    fn target_table(&self, dec: &mut BlockDecoder<'_>, database: &str) -> Option<QualifiedName> {
        let attr = dec.attr("to").filter(|a| !matches!(a.value, Value::Null))?;
        match &attr.value {
            Value::Ref(a) if a.kind == "table" && a.nested.is_empty() => self
                .symbol(a)
                .map(|s| QualifiedName::new(&s.database, &s.name)),
            Value::String(s) if !s.is_empty() => Some(match s.split_once('.') {
                Some((d, n)) => QualifiedName::new(d, n),
                None => QualifiedName::new(database, s),
            }),
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`to` must be a table reference or a `database.table` string, found {}",
                        other.type_name()
                    ),
                );
                None
            }
        }
    }

    pub(super) fn dictionary(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let (database, name) = self.qualified_name(&mut dec);
        if !self.declare_relation(ObjectKind::Dictionary, &database, &name, block.header_span) {
            dec.finish();
            return;
        }
        let r = ObjectRef::new(ObjectKind::Dictionary, [&database, &name]);
        let mut dict = Dictionary {
            name: name.clone(),
            ..Dictionary::default()
        };
        for mut child in dec.children("attribute") {
            if let Some(attribute) = self.attribute(&mut child) {
                if dict.attributes.iter().any(|a| a.name == attribute.name) {
                    self.error(
                        child.block().header_span,
                        format!("attribute `{}` is declared more than once", attribute.name),
                    );
                } else {
                    dict.attributes.push(attribute);
                }
            }
            child.finish();
        }
        dict.primary_key = match dec.attr("primary_key").map(|a| (&a.value, a.value_span)) {
            Some((Value::List(items), span)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::String(s) if !s.is_empty() => Some(s.clone()),
                    Value::Ref(a) if a.nested.len() == 1 && a.nested[0].0 == "attribute" => {
                        self.nested_db_name(a).map(|n| column_expr(&n))
                    }
                    other => {
                        self.error(
                            span,
                            format!(
                                "`primary_key` entries must be attribute references or strings, \
                                 found {}",
                                other.type_name()
                            ),
                        );
                        None
                    }
                })
                .collect(),
            Some((Value::String(s), span)) => ddl::split_list(s).unwrap_or_else(|e| {
                self.error(span, e.message);
                Vec::new()
            }),
            Some((Value::Null, _)) | None => {
                self.error(block.header_span, "`dictionary` requires `primary_key`");
                Vec::new()
            }
            Some((other, span)) => {
                self.error(
                    span,
                    format!("`primary_key` must be a list, found {}", other.type_name()),
                );
                Vec::new()
            }
        };
        dict.source = self
            .sql_attr(&mut dec, "source", true, sql_text)
            .unwrap_or_default();
        dict.layout = self
            .sql_attr(&mut dec, "layout", true, |v| match v {
                Value::Ident(w) => Ok(format!("{w}()")),
                other => sql_text(other),
            })
            .unwrap_or_default();
        dict.lifetime = self.sql_attr(&mut dec, "lifetime", false, |v| match v {
            Value::Number(Number::Int(n)) => Ok(n.to_string()),
            other => expression(other),
        });
        dict.range = self.sql_attr(&mut dec, "range", false, expression);
        dict.settings = self.settings_attr(&mut dec, "settings");
        dict.comment = self.comment(&mut dec);
        self.depends_on(&mut dec, &r);
        dec.finish();
        self.database_mut(&database).dictionaries.insert(name, dict);
    }

    fn attribute(&mut self, dec: &mut BlockDecoder<'a>) -> Option<DictionaryAttribute> {
        let name = self.block_name(dec)?;
        let data_type = self.sql_attr(dec, "type", true, sql_text)?;
        Some(DictionaryAttribute {
            name,
            data_type,
            default: self.sql_attr(dec, "default", false, literal),
            expression: self.sql_attr(dec, "expression", false, expression),
            hierarchical: dec.opt::<bool>("hierarchical").unwrap_or(false),
            bidirectional: dec.opt::<bool>("bidirectional").unwrap_or(false),
            injective: dec.opt::<bool>("injective").unwrap_or(false),
            is_object_id: dec.opt::<bool>("is_object_id").unwrap_or(false),
        })
    }

    pub(super) fn function(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.block_name(&mut dec) else {
            dec.finish();
            return;
        };
        if !self.declare(
            ObjectRef::new(ObjectKind::Function, [name.as_str()]),
            block.header_span,
        ) {
            dec.finish();
            return;
        }
        let parameters = self.string_list(&mut dec, "args").unwrap_or_default();
        let body = self.sql_attr(&mut dec, "body", true, expression);
        dec.finish();
        if let Some(body) = body {
            self.model.functions.insert(
                name.clone(),
                Function {
                    name,
                    parameters,
                    body,
                },
            );
        }
    }
}
