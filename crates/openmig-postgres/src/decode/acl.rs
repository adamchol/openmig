//! `grant` and `revoke` blocks.

use super::Decoder;
use super::keywords::PrivilegeKeyword;
use crate::model::{Acl, AclItem, Privilege, ViewColumn};
use openmig_hcl::{Address, Block, Value};

/// Privileges that apply to an object kind, in `GRANT ALL` order.
pub(crate) fn privileges_of(kind: &str, columns: bool) -> &'static [Privilege] {
    use Privilege as P;
    match kind {
        "table" | "view" | "materialized_view" if columns => {
            &[P::Select, P::Insert, P::Update, P::References]
        }
        "table" | "view" | "materialized_view" => &[
            P::Select,
            P::Insert,
            P::Update,
            P::Delete,
            P::Truncate,
            P::References,
            P::Trigger,
            P::Maintain,
        ],
        "sequence" => &[P::Usage, P::Select, P::Update],
        "schema" => &[P::Usage, P::Create],
        "function" | "procedure" => &[P::Execute],
        "enum" | "composite" | "domain" => &[P::Usage],
        _ => &[],
    }
}

/// Block kinds `on` accepts.
const GRANTABLE: &[&str] = &[
    "schema",
    "table",
    "view",
    "materialized_view",
    "sequence",
    "function",
    "procedure",
    "enum",
    "composite",
    "domain",
];

impl<'a> Decoder<'a> {
    /// A `grant` (`grant = true`) or `revoke` block.
    pub(super) fn privileges(&mut self, block: &'a Block, grant: bool) {
        let mut dec = self.decoder(block);
        let span = block.header_span;
        let privileges = self.keyword_list::<PrivilegeKeyword>(&mut dec, "privileges");
        if privileges.is_none() && !dec.has("privileges") {
            self.error(
                span,
                format!("`{}` requires the attribute `privileges`", block.kind),
            );
        }
        let targets = self.grant_targets(&mut dec);
        let role_attr = if grant { "to" } else { "from" };
        let roles = self.role_list(&mut dec, role_attr);
        if roles.is_none() && !dec.has(role_attr) {
            self.error(
                span,
                format!("`{}` requires the attribute `{role_attr}`", block.kind),
            );
        }
        let columns = if grant {
            self.grant_columns(&mut dec)
        } else {
            None
        };
        let grantable = grant && dec.opt::<bool>("with_grant_option").unwrap_or(false);
        dec.finish();
        let (Some(privileges), Some(targets), Some(roles)) = (privileges, targets, roles) else {
            return;
        };
        for target in &targets {
            let kind = target.kind.as_str();
            let allowed = privileges_of(kind, columns.is_some());
            let mut expanded: Vec<Privilege> = Vec::new();
            for p in &privileges {
                match p {
                    PrivilegeKeyword::All => expanded.extend_from_slice(allowed),
                    PrivilegeKeyword::One(p) if allowed.contains(p) => expanded.push(*p),
                    PrivilegeKeyword::One(p) => {
                        let on = if columns.is_some() {
                            format!("columns of {kind}")
                        } else {
                            kind.to_owned()
                        };
                        self.error(
                            span,
                            format!(
                                "privilege {p} does not apply to {on} `{}`",
                                self.shown_target(target)
                            ),
                        );
                    }
                }
            }
            let apply = |acl: &mut Acl| {
                for role in &roles {
                    for p in &expanded {
                        if grant {
                            acl.insert(AclItem {
                                grantee: role.clone(),
                                privilege: *p,
                                grantable,
                            });
                        } else {
                            acl.retain(|i| !(i.grantee == *role && i.privilege == *p));
                        }
                    }
                }
            };
            match &columns {
                Some(columns) => self.with_column_acls(target, columns, span, apply),
                None => {
                    if let Some(acl) = self.acl_mut(target) {
                        apply(acl);
                    }
                }
            }
        }
    }

    /// How a grant target is named in messages: `app.users`, `app.f(integer)`, `app`.
    fn shown_target(&self, address: &Address) -> String {
        match self.symbol(address) {
            Some(s) if s.schema.is_empty() => s.name.clone(),
            Some(s) => format!("{}.{}", s.schema, s.signature.as_ref().unwrap_or(&s.name)),
            None => address.to_string(),
        }
    }

    fn grant_targets(&self, dec: &mut openmig_hcl::BlockDecoder<'_>) -> Option<Vec<Address>> {
        let Some(attr) = dec.attr("on").filter(|a| !matches!(a.value, Value::Null)) else {
            self.error(
                dec.block().header_span,
                format!("`{}` requires the attribute `on`", dec.block().kind),
            );
            return None;
        };
        let Value::List(items) = &attr.value else {
            self.error(
                attr.value_span,
                format!(
                    "`on` must be a list of references, found {}",
                    attr.value.type_name()
                ),
            );
            return None;
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::Ref(a) if a.nested.is_empty() && GRANTABLE.contains(&a.kind.as_str()) => {
                    out.push(a.clone());
                }
                Value::Ref(a) => self.error(
                    attr.value_span,
                    format!(
                        "privileges cannot be granted on `{a}`; `on` accepts references to {}",
                        GRANTABLE.join(", ")
                    ),
                ),
                other => self.error(
                    attr.value_span,
                    format!(
                        "`on` entries must be references, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        Some(out)
    }

    /// `columns` of a grant: column names, or references to table columns.
    fn grant_columns(&self, dec: &mut openmig_hcl::BlockDecoder<'_>) -> Option<Vec<String>> {
        let attr = dec
            .attr("columns")
            .filter(|a| !matches!(a.value, Value::Null))?;
        let Value::List(items) = &attr.value else {
            self.error(
                attr.value_span,
                format!("`columns` must be a list, found {}", attr.value.type_name()),
            );
            return None;
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::String(s) if !s.is_empty() => out.push(s.clone()),
                Value::Ref(a) => match self.column_ref(a) {
                    Some((_, column)) => out.push(column),
                    None => self.error(attr.value_span, format!("`{a}` is not a table column")),
                },
                other => self.error(
                    attr.value_span,
                    format!(
                        "`columns` entries must be column names, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        Some(out)
    }

    /// The ACL of the object `address` names.
    fn acl_mut(&mut self, address: &Address) -> Option<&mut Acl> {
        let symbol = self.symbol(address)?.clone();
        if address.kind == "schema" {
            return self.db.schemas.get_mut(&symbol.name).map(|s| &mut s.acl);
        }
        let schema = self.db.schemas.get_mut(&symbol.schema)?;
        let name = &symbol.name;
        match address.kind.as_str() {
            "table" => schema.tables.get_mut(name).map(|t| &mut t.acl),
            "view" => schema.views.get_mut(name).map(|v| &mut v.acl),
            "materialized_view" => schema.materialized_views.get_mut(name).map(|m| &mut m.acl),
            "sequence" => schema.sequences.get_mut(name).map(|q| &mut q.acl),
            "function" => schema
                .functions
                .get_mut(symbol.signature.as_ref()?)
                .map(|f| &mut f.acl),
            "procedure" => schema
                .procedures
                .get_mut(symbol.signature.as_ref()?)
                .map(|f| &mut f.acl),
            "enum" => schema.enums.get_mut(name).map(|e| &mut e.acl),
            "composite" => schema.composites.get_mut(name).map(|c| &mut c.acl),
            "domain" => schema.domains.get_mut(name).map(|d| &mut d.acl),
            _ => None,
        }
    }

    /// Applies `apply` to the ACL of each of `columns` of a table, view or materialized view.
    /// View columns are not derived from the query, so they are added as needed.
    fn with_column_acls(
        &mut self,
        address: &Address,
        columns: &[String],
        span: openmig_hcl::Span,
        mut apply: impl FnMut(&mut Acl),
    ) {
        let Some(symbol) = self.symbol(address).cloned() else {
            return;
        };
        let Some(schema) = self.db.schemas.get_mut(&symbol.schema) else {
            return;
        };
        match address.kind.as_str() {
            "table" => {
                let Some(table) = schema.tables.get_mut(&symbol.name) else {
                    return;
                };
                let mut unknown = Vec::new();
                for c in columns {
                    match table.column_mut(c) {
                        Some(column) => apply(&mut column.acl),
                        None => unknown.push(c.clone()),
                    }
                }
                for c in unknown {
                    self.error(
                        span,
                        format!("unknown column `{c}` in `{}`", symbol.qualified()),
                    );
                }
            }
            "view" | "materialized_view" => {
                let view_columns = if address.kind == "view" {
                    schema.views.get_mut(&symbol.name).map(|v| &mut v.columns)
                } else {
                    schema
                        .materialized_views
                        .get_mut(&symbol.name)
                        .map(|m| &mut m.columns)
                };
                let Some(view_columns) = view_columns else {
                    return;
                };
                for c in columns {
                    if !view_columns.iter().any(|v| &v.name == c) {
                        view_columns.push(ViewColumn {
                            name: c.clone(),
                            ..ViewColumn::default()
                        });
                    }
                    if let Some(column) = view_columns.iter_mut().find(|v| &v.name == c) {
                        apply(&mut column.acl);
                    }
                }
            }
            kind => self.error(
                span,
                format!(
                    "column privileges do not apply to {kind} `{}`",
                    self.shown_target(address)
                ),
            ),
        }
    }
}
