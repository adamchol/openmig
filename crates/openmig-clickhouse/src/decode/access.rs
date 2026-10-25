//! Roles, grants and row policies.

use super::Decoder;
use super::values::expression;
use crate::model::{Grant, Grantee, Role, RoleGrant, RoleSet, RowPolicy, RowPolicyId};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::{Block, BlockDecoder, Value};
use std::collections::BTreeSet;

/// What a grant applies to.
enum Target {
    Global,
    Database(String),
    Table(String, String),
}

impl<'a> Decoder<'a> {
    pub(super) fn role(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.block_name(&mut dec) else {
            dec.finish();
            return;
        };
        if !self.declare(
            ObjectRef::new(ObjectKind::Role, [name.as_str()]),
            block.header_span,
        ) {
            dec.finish();
            return;
        }
        let storage = match dec.attr("storage").map(|a| (&a.value, a.value_span)) {
            Some((Value::Ident(s) | Value::String(s), _)) => s.clone(),
            Some((Value::Null, _)) | None => String::new(),
            Some((other, span)) => {
                self.error(
                    span,
                    format!(
                        "`storage` must be a user directory name such as local_directory or \
                         replicated, found {}",
                        other.type_name()
                    ),
                );
                String::new()
            }
        };
        self.member_of(&mut dec, &name);
        dec.finish();
        self.model
            .roles
            .insert(name.clone(), Role { name, storage });
    }

    /// `member_of = [role.a, { role = role.b, admin = true }]`: role memberships of `member`.
    fn member_of(&mut self, dec: &mut BlockDecoder<'_>, member: &str) {
        let Some(a) = dec
            .attr("member_of")
            .filter(|a| !matches!(a.value, Value::Null))
        else {
            return;
        };
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!("`member_of` must be a list, found {}", a.value.type_name()),
            );
            return;
        };
        for item in items {
            let (role, admin) = match item {
                Value::Object(map) => {
                    let admin = match map.get("admin") {
                        None | Some(Value::Null) => false,
                        Some(Value::Bool(b)) => *b,
                        Some(_) => {
                            self.error(a.value_span, "`admin` in `member_of` must be a bool");
                            false
                        }
                    };
                    if let Some(key) = map.keys().find(|k| *k != "role" && *k != "admin") {
                        self.error(
                            a.value_span,
                            format!("unknown key `{key}` in `member_of` (expected role, admin)"),
                        );
                    }
                    match map.get("role") {
                        Some(role) => (self.role_name(role), admin),
                        None => {
                            self.error(a.value_span, "`member_of` entries need `role`");
                            continue;
                        }
                    }
                }
                other => (self.role_name(other), false),
            };
            match role {
                Ok(role) => {
                    self.mentioned_roles.insert(role.clone());
                    self.model.role_grants.push(RoleGrant {
                        grantee: Grantee::Role(member.to_owned()),
                        role,
                        admin_option: admin,
                    });
                }
                Err(message) => self.error(a.value_span, message),
            }
        }
    }

    /// A role name from a role reference or a string.
    fn role_name(&self, value: &Value) -> Result<String, String> {
        match value {
            Value::Ref(a) if a.kind == "role" && a.nested.is_empty() => self
                .symbol(a)
                .map(|s| s.name.clone())
                .ok_or_else(|| format!("unknown role `{a}`")),
            Value::String(s) if !s.is_empty() => Ok(s.clone()),
            Value::Ident(w) => Err(format!(
                "`{w}` is not a role: use a role reference (`role.{w}`) or a string"
            )),
            other => Err(format!(
                "expected a role reference or a role name, found {}",
                other.type_name()
            )),
        }
    }

    /// Roles of `attr`, recorded as mentioned. `ALL` is accepted when `allow_all`.
    fn roles(
        &mut self,
        dec: &mut BlockDecoder<'_>,
        attr: &str,
        allow_all: bool,
    ) -> Option<(bool, Vec<String>)> {
        let a = dec.attr(attr).filter(|a| !matches!(a.value, Value::Null))?;
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!(
                    "`{attr}` must be a list of roles, found {}",
                    a.value.type_name()
                ),
            );
            return None;
        };
        let mut all = false;
        let mut out = Vec::new();
        for item in items {
            if allow_all && matches!(item, Value::Ident(w) if w.eq_ignore_ascii_case("ALL")) {
                all = true;
                continue;
            }
            match self.role_name(item) {
                Ok(name) => {
                    self.mentioned_roles.insert(name.clone());
                    out.push(name);
                }
                Err(message) => self.error(a.value_span, message),
            }
        }
        out.sort();
        out.dedup();
        Some((all, out))
    }

    pub(super) fn grant(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let privileges = self.privileges(&mut dec);
        let targets = self.targets(&mut dec);
        let columns = self.grant_columns(&mut dec);
        let grantees = match self.roles(&mut dec, "to", false) {
            Some((_, roles)) if !roles.is_empty() => roles,
            Some(_) => {
                self.error(block.header_span, "`to` cannot be empty");
                Vec::new()
            }
            None => {
                self.error(block.header_span, "`grant` requires the attribute `to`");
                Vec::new()
            }
        };
        let grant_option = dec.opt::<bool>("with_grant_option").unwrap_or(false);
        dec.finish();
        let (Some(privileges), Some(targets)) = (privileges, targets) else {
            return;
        };
        if !columns.is_empty() && targets.iter().any(|t| !matches!(t, Target::Table(..))) {
            self.error(
                block.header_span,
                "`columns` can only be granted on tables, views and dictionaries",
            );
            return;
        }
        for target in &targets {
            for grantee in &grantees {
                let (database, table) = match target {
                    Target::Global => (None, None),
                    Target::Database(d) => (Some(d.clone()), None),
                    Target::Table(d, t) => (Some(d.clone()), Some(t.clone())),
                };
                let grant = Grant {
                    grantee: Grantee::Role(grantee.clone()),
                    database,
                    table,
                    parameter: None,
                    columns: columns.clone(),
                    partial_revoke: false,
                    grant_option,
                    privileges: privileges.clone(),
                };
                self.origins
                    .entry(grant.object_ref())
                    .or_insert(block.header_span);
                match self.model.grants.iter_mut().find(|g| {
                    g.grantee == grant.grantee
                        && g.database == grant.database
                        && g.table == grant.table
                        && g.columns == grant.columns
                        && g.grant_option == grant.grant_option
                }) {
                    Some(existing) => {
                        existing.privileges.extend(grant.privileges);
                        collapse_all(&mut existing.privileges);
                    }
                    None => self.model.grants.push(grant),
                }
            }
        }
    }

    fn privileges(&self, dec: &mut BlockDecoder<'_>) -> Option<BTreeSet<String>> {
        let Some(a) = dec
            .attr("privileges")
            .filter(|a| !matches!(a.value, Value::Null))
        else {
            self.error(
                dec.block().header_span,
                "`grant` requires the attribute `privileges`",
            );
            return None;
        };
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!("`privileges` must be a list, found {}", a.value.type_name()),
            );
            return None;
        };
        let mut out = BTreeSet::new();
        for item in items {
            match item {
                Value::Ident(w) => {
                    out.insert(privilege_name(&w.replace('_', " ")));
                }
                Value::String(s) if !s.trim().is_empty() => {
                    out.insert(privilege_name(s.trim()));
                }
                other => self.error(
                    a.value_span,
                    format!(
                        "`privileges` entries must be identifiers or strings, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        if out.is_empty() {
            self.error(a.value_span, "`privileges` cannot be empty");
            return None;
        }
        collapse_all(&mut out);
        Some(out)
    }

    fn targets(&self, dec: &mut BlockDecoder<'_>) -> Option<Vec<Target>> {
        let Some(a) = dec.attr("on").filter(|a| !matches!(a.value, Value::Null)) else {
            self.error(
                dec.block().header_span,
                "`grant` requires the attribute `on`",
            );
            return None;
        };
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!(
                    "`on` must be a list of references or strings, found {}",
                    a.value.type_name()
                ),
            );
            return None;
        };
        let mut out = Vec::new();
        for item in items {
            let target = match item {
                Value::Ref(r) if r.nested.is_empty() => match (r.kind.as_str(), self.symbol(r)) {
                    ("database", Some(s)) => Some(Target::Database(s.name.clone())),
                    ("database", None) => Some(Target::Database(r.leaf_label().to_owned())),
                    ("table" | "view" | "materialized_view" | "dictionary", Some(s)) => {
                        Some(Target::Table(s.database.clone(), s.name.clone()))
                    }
                    _ => None,
                },
                Value::String(s) => match s.trim() {
                    "*" | "*.*" => Some(Target::Global),
                    text => match text.split_once('.') {
                        Some((d, "*")) if !d.is_empty() => Some(Target::Database(d.to_owned())),
                        Some((d, t)) if !d.is_empty() && !t.is_empty() => {
                            Some(Target::Table(d.to_owned(), t.to_owned()))
                        }
                        _ => None,
                    },
                },
                _ => None,
            };
            match target {
                Some(t) => out.push(t),
                None => self.error(
                    a.value_span,
                    "`on` entries must be references to databases, tables, views, materialized \
                     views or dictionaries, or strings such as \"*.*\", \"db.*\" and \"db.table\"",
                ),
            }
        }
        Some(out)
    }

    fn grant_columns(&self, dec: &mut BlockDecoder<'_>) -> Vec<String> {
        let Some(a) = dec
            .attr("columns")
            .filter(|a| !matches!(a.value, Value::Null))
        else {
            return Vec::new();
        };
        let Value::List(items) = &a.value else {
            self.error(
                a.value_span,
                format!("`columns` must be a list, found {}", a.value.type_name()),
            );
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for item in items {
            match item {
                Value::String(s) if !s.is_empty() => out.push(s.clone()),
                Value::Ref(r) if r.nested.len() == 1 && r.nested[0].0 == "column" => {
                    if let Some(name) = self.nested_db_name(r) {
                        out.push(name);
                    }
                }
                other => self.error(
                    a.value_span,
                    format!(
                        "`columns` entries must be column references or names, found {}",
                        other.type_name()
                    ),
                ),
            }
        }
        out.sort();
        out.dedup();
        out
    }

    pub(super) fn row_policy(&mut self, block: &'a Block) {
        let mut dec = self.decoder(block);
        let Some(name) = self.block_name(&mut dec) else {
            dec.finish();
            return;
        };
        let table = match dec.attr("on").map(|a| (&a.value, a.value_span)) {
            Some((Value::Ref(r), _)) if r.nested.is_empty() && r.kind == "table" => {
                self.symbol(r).map(|s| (s.database.clone(), s.name.clone()))
            }
            Some((Value::String(s), span)) => match s.split_once('.') {
                Some((d, t)) if !d.is_empty() && !t.is_empty() => {
                    Some((d.to_owned(), t.to_owned()))
                }
                _ => {
                    self.error(span, "`on` must be a table reference or `database.table`");
                    None
                }
            },
            Some((other, span)) => {
                self.error(
                    span,
                    format!(
                        "`on` must be a table reference, found {}",
                        other.type_name()
                    ),
                );
                None
            }
            None => {
                self.error(
                    block.header_span,
                    "`row_policy` requires the attribute `on`",
                );
                None
            }
        };
        let filter = self.sql_attr(&mut dec, "using", false, expression);
        let restrictive = match dec.attr("as").map(|a| (&a.value, a.value_span)) {
            Some((Value::Ident(w) | Value::String(w), span)) => match w.to_uppercase().as_str() {
                "RESTRICTIVE" => true,
                "PERMISSIVE" => false,
                _ => {
                    self.error(span, "`as` must be PERMISSIVE or RESTRICTIVE");
                    false
                }
            },
            Some((Value::Null, _)) | None => false,
            Some((_, span)) => {
                self.error(span, "`as` must be PERMISSIVE or RESTRICTIVE");
                false
            }
        };
        let (all, names) = self.roles(&mut dec, "to", true).unwrap_or_default();
        let except = self
            .roles(&mut dec, "except", false)
            .map(|(_, r)| r)
            .unwrap_or_default();
        dec.finish();
        let Some((database, table)) = table else {
            return;
        };
        let id = RowPolicyId {
            database,
            table,
            name,
        };
        if !self.declare(id.object_ref(), block.header_span) {
            return;
        }
        self.model.row_policies.insert(
            id,
            RowPolicy {
                filter,
                restrictive,
                to: RoleSet { all, names, except },
                storage: String::new(),
            },
        );
    }
}

/// A privilege name as ClickHouse spells it: upper case unless written in mixed case
/// (`dictGet`); `ALL PRIVILEGES` is `ALL`.
fn privilege_name(text: &str) -> String {
    let has_upper = text.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = text.chars().any(|c| c.is_ascii_lowercase());
    let name = if has_upper && has_lower {
        text.to_owned()
    } else {
        text.to_uppercase()
    };
    match name.as_str() {
        "ALL PRIVILEGES" => "ALL".to_owned(),
        _ => name,
    }
}

/// `ALL` implies every other privilege.
fn collapse_all(privileges: &mut BTreeSet<String>) {
    if privileges.contains("ALL") {
        privileges.retain(|p| p == "ALL");
    }
}
