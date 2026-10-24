//! Schemas, cluster-wide objects, types, sequences, views and top-level triggers and policies.

use super::table::{SequenceParams, sequence_options};
use super::types::{TypeContext, default_sql};
use super::{ColumnOwner, Decoder, Symbol, is_system_schema};
use crate::model::{
    ColumnRef, Composite, CompositeField, Domain, DomainCheck, Enum, EventTrigger, Extension,
    MaterializedView, Membership, PUBLIC, Role, Sequence, View, ViewCheckOption, ViewColumn,
};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::{Block, BlockDecoder, Value};

/// A view query without surrounding whitespace and without a trailing `;`, so it can be embedded
/// in `CREATE VIEW ... AS <query>`.
pub(crate) fn view_query(text: &str) -> String {
    text.trim().trim_end_matches(';').trim_end().to_owned()
}

impl<'a> Decoder<'a> {
    pub(super) fn schema_block(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let name = self.nested_name(&mut dec).unwrap_or_default();
        let r = ObjectRef::new(ObjectKind::Schema, [&name]);
        if name.is_empty() || !self.declare(r.clone(), block.header_span, block.each_key.is_some())
        {
            return;
        }
        if is_system_schema(&name) {
            self.error(
                block.header_span,
                format!("schema `{name}` is a system schema and cannot be declared"),
            );
            return;
        }
        let mut schema = self.new_schema(&name);
        if dec.has("comment") {
            schema.comment = self.comment(&mut dec);
        }
        self.renamed_from(&mut dec, ObjectKind::Schema, &[&name], false);
        self.depends_on(&mut dec, &r);
        dec.finish();
        self.db.schemas.insert(name, schema);
    }

    pub(super) fn role(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.nested_name(&mut dec) else {
            return;
        };
        let r = ObjectRef::new(ObjectKind::Role, [&name]);
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        let mut role = Role::new(&name);
        role.login = dec.opt("login").unwrap_or(false);
        role.superuser = dec.opt("superuser").unwrap_or(false);
        role.createdb = dec.opt("createdb").unwrap_or(false);
        role.createrole = dec.opt("createrole").unwrap_or(false);
        role.inherit = dec.opt("inherit").unwrap_or(true);
        role.replication = dec.opt("replication").unwrap_or(false);
        role.bypassrls = dec.opt("bypassrls").unwrap_or(false);
        role.connection_limit = dec.opt("connection_limit").unwrap_or(-1);
        role.valid_until = dec.opt::<String>("valid_until");
        role.comment = self.comment(&mut dec);
        if let Some(attr) = dec
            .attr("member_of")
            .filter(|a| !matches!(a.value, Value::Null))
        {
            let items = match &attr.value {
                Value::List(items) => items.as_slice(),
                other => {
                    self.error(
                        attr.value_span,
                        format!(
                            "`member_of` must be a list of roles, found {}",
                            other.type_name()
                        ),
                    );
                    &[]
                }
            };
            for item in items {
                let default = Membership {
                    admin: false,
                    inherit: role.inherit,
                    set: true,
                };
                let parsed = match item {
                    Value::Object(map) => {
                        let mut membership = default;
                        let mut target = Err("a membership needs `role`".to_owned());
                        for (key, value) in map {
                            match (key.as_str(), value) {
                                ("role", v) => target = self.role_name(v),
                                ("admin", Value::Bool(b)) => membership.admin = *b,
                                ("inherit", Value::Bool(b)) => membership.inherit = *b,
                                ("set", Value::Bool(b)) => membership.set = *b,
                                (k, _) => {
                                    target = Err(format!(
                                        "a membership is `{{ role = ..., admin = bool, inherit = \
                                         bool, set = bool }}`; `{k}` is not valid"
                                    ));
                                    break;
                                }
                            }
                        }
                        target.map(|t| (t, membership))
                    }
                    other => self.role_name(other).map(|t| (t, default)),
                };
                match parsed {
                    Ok((target, _))
                        if target == PUBLIC || super::special_role(&target).is_some() =>
                    {
                        self.error(attr.value_span, format!("cannot be a member of `{target}`"));
                    }
                    Ok((target, membership)) => {
                        self.mentioned_roles.insert(target.clone());
                        role.member_of.insert(target, membership);
                    }
                    Err(message) => self.error(attr.value_span, message),
                }
            }
        }
        dec.finish();
        self.db.roles.insert(name, role);
    }

    pub(super) fn extension(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.nested_name(&mut dec) else {
            return;
        };
        let schema = match dec.value("schema") {
            None => self.settings.default_schema.clone(),
            Some(Value::Ref(a)) if a.kind == "schema" => {
                self.symbol(a).map(|s| s.name.clone()).unwrap_or_default()
            }
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(other) => {
                self.error(
                    block.header_span,
                    format!(
                        "`schema` must be a schema reference or a string, found {}",
                        other.type_name()
                    ),
                );
                String::new()
            }
        };
        let version = dec.opt::<String>("version").unwrap_or_default();
        let comment = self.comment(&mut dec);
        let r = ObjectRef::new(ObjectKind::Extension, [&name]);
        self.depends_on(&mut dec, &r);
        dec.finish();
        // plpgsql is installed in every database and never part of a model.
        if name == "plpgsql" || !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        self.ensure_schema(&schema, Some(block.header_span));
        self.db.extensions.insert(
            name.clone(),
            Extension {
                name,
                schema,
                version,
                comment,
            },
        );
    }

    pub(super) fn enum_type(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::Enum, [&qn.schema, &qn.name]);
        let values = dec.req::<Vec<String>>("values");
        let comment = self.comment(&mut dec);
        self.renamed_from(&mut dec, ObjectKind::Enum, &[&qn.schema, &qn.name], true);
        self.depends_on(&mut dec, &r);
        dec.finish();
        let Some(values) = values else {
            return;
        };
        for (i, v) in values.iter().enumerate() {
            if values[..i].contains(v) {
                self.error(
                    block.header_span,
                    format!("enum value `{v}` is listed twice"),
                );
            }
        }
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        let acl = self.default_acl(ObjectKind::Enum);
        self.schema_mut(&qn.schema, block.header_span).enums.insert(
            qn.name.clone(),
            Enum {
                name: qn.name,
                values,
                comment,
                acl,
                ..Enum::default()
            },
        );
    }

    pub(super) fn composite(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::Composite, [&qn.schema, &qn.name]);
        if !self.declare(r.clone(), block.header_span, block.each_key.is_some()) {
            return;
        }
        let mut fields = Vec::new();
        for mut f in dec.children("field") {
            let name = self.nested_name(&mut f);
            let data_type = self.type_attr(&mut f, TypeContext::Column);
            let collation = self.collation(&mut f, "collate");
            if let (Some(name), Some(data_type)) = (name, data_type) {
                let fr = ObjectRef::new(ObjectKind::Column, [&qn.schema, &qn.name, &name]);
                if self.declare(fr, f.block().header_span, f.block().each_key.is_some()) {
                    fields.push(CompositeField {
                        name,
                        data_type,
                        collation,
                    });
                }
            }
            f.finish();
        }
        let comment = self.comment(&mut dec);
        self.renamed_from(
            &mut dec,
            ObjectKind::Composite,
            &[&qn.schema, &qn.name],
            true,
        );
        self.depends_on(&mut dec, &r);
        dec.finish();
        let acl = self.default_acl(ObjectKind::Composite);
        self.schema_mut(&qn.schema, block.header_span)
            .composites
            .insert(
                qn.name.clone(),
                Composite {
                    name: qn.name,
                    fields,
                    comment,
                    acl,
                    ..Composite::default()
                },
            );
    }

    pub(super) fn domain(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::Domain, [&qn.schema, &qn.name]);
        if !self.declare(r.clone(), block.header_span, block.each_key.is_some()) {
            return;
        }
        let data_type = self.type_attr(&mut dec, TypeContext::Column);
        let not_null = dec.opt::<bool>("null") == Some(false);
        let default = match (dec.attr("default"), &data_type) {
            (Some(attr), Some(t)) => match default_sql(&attr.value, t) {
                Ok(text) => text,
                Err(message) => {
                    self.error(attr.value_span, message);
                    None
                }
            },
            _ => None,
        };
        let collation = self.collation(&mut dec, "collate");
        let mut checks = std::collections::BTreeMap::new();
        for mut c in dec.children("check") {
            let name = self.nested_name(&mut c);
            let expr = c.req::<String>("expr");
            let validated = !c.opt("not_valid").unwrap_or(false);
            let comment = self.comment(&mut c);
            if let (Some(name), Some(expr)) = (name, expr) {
                let cr = ObjectRef::new(ObjectKind::Check, [&qn.schema, &qn.name, &name]);
                if self.declare(cr, c.block().header_span, c.block().each_key.is_some()) {
                    checks.insert(
                        name.clone(),
                        DomainCheck {
                            name,
                            expr,
                            validated,
                            comment,
                        },
                    );
                }
            }
            c.finish();
        }
        let comment = self.comment(&mut dec);
        self.renamed_from(&mut dec, ObjectKind::Domain, &[&qn.schema, &qn.name], true);
        self.depends_on(&mut dec, &r);
        dec.finish();
        let Some(data_type) = data_type else {
            return;
        };
        let acl = self.default_acl(ObjectKind::Domain);
        self.schema_mut(&qn.schema, block.header_span)
            .domains
            .insert(
                qn.name.clone(),
                Domain {
                    name: qn.name,
                    data_type,
                    not_null,
                    default,
                    collation,
                    checks,
                    comment,
                    acl,
                    ..Domain::default()
                },
            );
    }

    pub(super) fn sequence(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::Sequence, [&qn.schema, &qn.name]);
        if !self.declare(r.clone(), block.header_span, block.each_key.is_some()) {
            return;
        }
        let data_type = if dec.has("type") {
            self.type_attr(&mut dec, TypeContext::Column)
                .unwrap_or_else(|| "bigint".into())
        } else {
            "bigint".to_owned()
        };
        if !matches!(data_type.as_str(), "smallint" | "integer" | "bigint") {
            self.error(
                block.header_span,
                format!("a sequence type must be smallint, integer or bigint, found `{data_type}`"),
            );
        }
        let params = SequenceParams {
            start: dec.opt("start"),
            increment: dec.opt("increment"),
            min: dec.opt("min"),
            max: dec.opt("max"),
            cache: dec.opt("cache"),
            cycle: dec.opt("cycle"),
        };
        let owned_by = match dec
            .attr("owned_by")
            .filter(|a| !matches!(a.value, Value::Null))
        {
            None => None,
            Some(attr) => match &attr.value {
                Value::Ref(a) => match self.column_ref(a) {
                    Some((table, column)) => Some(ColumnRef {
                        schema: table.schema,
                        table: table.name,
                        column,
                    }),
                    None => {
                        self.error(attr.value_span, format!("`{a}` is not a table column"));
                        None
                    }
                },
                other => {
                    self.error(
                        attr.value_span,
                        format!(
                            "`owned_by` must be a column reference, found {}",
                            other.type_name()
                        ),
                    );
                    None
                }
            },
        };
        let comment = self.comment(&mut dec);
        self.renamed_from(
            &mut dec,
            ObjectKind::Sequence,
            &[&qn.schema, &qn.name],
            true,
        );
        self.depends_on(&mut dec, &r);
        dec.finish();
        let acl = self.default_acl(ObjectKind::Sequence);
        let options = sequence_options(&data_type, &params);
        self.schema_mut(&qn.schema, block.header_span)
            .sequences
            .insert(
                qn.name.clone(),
                Sequence {
                    name: qn.name,
                    data_type,
                    options,
                    owned_by,
                    comment,
                    acl,
                    ..Sequence::default()
                },
            );
    }

    /// `column "x" { comment = "..." }` blocks of views and materialized views.
    fn view_columns(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        schema: &str,
        view: &str,
    ) -> Vec<ViewColumn> {
        let mut columns = Vec::new();
        for mut c in dec.children("column") {
            if let Some(name) = self.nested_name(&mut c) {
                let comment = self.comment(&mut c);
                let r = ObjectRef::new(ObjectKind::Column, [schema, view, &name]);
                if self.declare(r, c.block().header_span, c.block().each_key.is_some()) {
                    columns.push(ViewColumn {
                        name,
                        comment,
                        ..ViewColumn::default()
                    });
                }
            }
            c.finish();
        }
        columns
    }

    pub(super) fn view(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::View, [&qn.schema, &qn.name]);
        if !self.declare(r.clone(), block.header_span, block.each_key.is_some()) {
            return;
        }
        let query = dec.req::<String>("as").map(|q| view_query(&q));
        let check_option: Option<ViewCheckOption> = dec.keyword("check_option");
        let view = View {
            name: qn.name.clone(),
            query: query.unwrap_or_default(),
            columns: self.view_columns(&mut dec, &qn.schema, &qn.name),
            security_invoker: dec.opt("security_invoker").unwrap_or(false),
            security_barrier: dec.opt("security_barrier").unwrap_or(false),
            check_option,
            comment: self.comment(&mut dec),
            acl: self.default_acl(ObjectKind::View),
            ..View::default()
        };
        self.renamed_from(&mut dec, ObjectKind::View, &[&qn.schema, &qn.name], true);
        self.depends_on(&mut dec, &r);
        dec.finish();
        self.schema_mut(&qn.schema, block.header_span)
            .views
            .insert(qn.name.clone(), view);
    }

    pub(super) fn materialized_view(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let r = ObjectRef::new(ObjectKind::MaterializedView, [&qn.schema, &qn.name]);
        if !self.declare(r.clone(), block.header_span, block.each_key.is_some()) {
            return;
        }
        let query = dec.req::<String>("as").map(|q| view_query(&q));
        let mut view = MaterializedView {
            name: qn.name.clone(),
            query: query.unwrap_or_default(),
            columns: self.view_columns(&mut dec, &qn.schema, &qn.name),
            with_data: dec.opt("with_data").unwrap_or(true),
            storage_params: self.options(&mut dec, "storage_params"),
            comment: self.comment(&mut dec),
            acl: self.default_acl(ObjectKind::MaterializedView),
            ..MaterializedView::default()
        };
        let owner = ColumnOwner {
            address: block.address.as_ref(),
            relation: &qn,
            columns: None,
        };
        for mut d in dec.children("index") {
            if let Some((name, index, key_names)) = self.index(&mut d, &owner) {
                self.add_index(&mut view.indexes, &qn, name, index, key_names, &d, true);
            }
            d.finish();
        }
        self.renamed_from(
            &mut dec,
            ObjectKind::MaterializedView,
            &[&qn.schema, &qn.name],
            true,
        );
        self.depends_on(&mut dec, &r);
        dec.finish();
        self.schema_mut(&qn.schema, block.header_span)
            .materialized_views
            .insert(qn.name.clone(), view);
    }

    pub(super) fn event_trigger(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.nested_name(&mut dec) else {
            return;
        };
        let r = ObjectRef::new(ObjectKind::EventTrigger, [&name]);
        let event = match dec.value("on") {
            Some(Value::Ident(w) | Value::String(w)) => Some(w.to_ascii_lowercase()),
            Some(other) => {
                self.error(
                    block.header_span,
                    format!("`on` must be an event name, found {}", other.type_name()),
                );
                None
            }
            None => {
                self.error(
                    block.header_span,
                    "`event_trigger` requires the attribute `on`",
                );
                None
            }
        };
        let mut tags = dec.opt::<Vec<String>>("tags").unwrap_or_default();
        tags.sort();
        tags.dedup();
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
                self.error(
                    block.header_span,
                    "`event_trigger` requires the attribute `execute`",
                );
                None
            }
        };
        let enabled = self.enabled(&mut dec);
        let comment = self.comment(&mut dec);
        self.depends_on(&mut dec, &r);
        dec.finish();
        let (Some(event), Some(function)) = (event, function) else {
            return;
        };
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        self.db.event_triggers.insert(
            name.clone(),
            EventTrigger {
                name,
                event,
                tags,
                function,
                enabled,
                comment,
                owner: String::new(),
            },
        );
    }

    /// The relation a top-level trigger or policy is `on`.
    fn on_relation(
        &self,
        dec: &mut BlockDecoder<'_>,
        kinds: &[&str],
    ) -> Option<(String, Symbol, openmig_hcl::Address)> {
        let Some(attr) = dec.attr("on").filter(|a| !matches!(a.value, Value::Null)) else {
            self.error(
                dec.block().header_span,
                format!(
                    "a top-level `{}` requires the attribute `on`",
                    dec.block().kind
                ),
            );
            return None;
        };
        match &attr.value {
            Value::Ref(a) if a.nested.is_empty() && kinds.contains(&a.kind.as_str()) => {
                let symbol = self.symbol(a)?.clone();
                Some((a.kind.clone(), symbol, a.clone()))
            }
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`on` must be a reference to a {}, found {}",
                        kinds.join(" or "),
                        other.type_name()
                    ),
                );
                None
            }
        }
    }

    pub(super) fn top_level_trigger(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some((kind, symbol, address)) = self.on_relation(&mut dec, &["table", "view"]) else {
            return;
        };
        let qn = symbol.qualified();
        let columns: Option<Vec<String>> = self
            .db
            .table(&qn)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect());
        let owner = ColumnOwner {
            address: Some(&address),
            relation: &qn,
            columns: columns.as_deref(),
        };
        let trigger = self.trigger(&mut dec, &owner);
        dec.finish();
        let Some(trigger) = trigger else {
            return;
        };
        let r = ObjectRef::new(ObjectKind::Trigger, [&qn.schema, &qn.name, &trigger.name]);
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        let Some(schema) = self.db.schemas.get_mut(&qn.schema) else {
            return;
        };
        let triggers = if kind == "table" {
            schema.tables.get_mut(&qn.name).map(|t| &mut t.triggers)
        } else {
            schema.views.get_mut(&qn.name).map(|v| &mut v.triggers)
        };
        if let Some(triggers) = triggers {
            triggers.insert(trigger.name.clone(), trigger);
        }
    }

    pub(super) fn top_level_policy(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some((_, symbol, _)) = self.on_relation(&mut dec, &["table"]) else {
            return;
        };
        let qn = symbol.qualified();
        let policy = self.policy(&mut dec);
        dec.finish();
        let Some(policy) = policy else {
            return;
        };
        let r = ObjectRef::new(ObjectKind::Policy, [&qn.schema, &qn.name, &policy.name]);
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        if let Some(table) = self
            .db
            .schemas
            .get_mut(&qn.schema)
            .and_then(|s| s.tables.get_mut(&qn.name))
        {
            table.policies.insert(policy.name.clone(), policy);
        }
    }
}
