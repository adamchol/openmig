//! `grant` and `revoke` blocks for the difference between each object's privileges and the
//! defaults the decoder starts from.

use super::Encoder;
use super::values::ident;
use crate::decode::{object_default_acl, schema_defaults};
use crate::model::{Acl, Privilege, Schema, ViewColumn};
use openmig_core::ObjectKind;
use openmig_hcl::Address;
use openmig_hcl::write::{WBlock, WExpr};
use std::collections::{BTreeMap, BTreeSet};

/// An object with privileges: its reference, its privileges, the defaults and its columns.
struct Target<'a> {
    address: Address,
    acl: &'a Acl,
    default: Acl,
    columns: Vec<(&'a str, &'a Acl)>,
}

/// One `grant` block: privileges, columns, grantees, with grant option.
type GrantKey = (Vec<Privilege>, Vec<String>, Vec<String>, bool);
/// One `revoke` block: privileges and grantees.
type RevokeKey = (Vec<Privilege>, Vec<String>);

/// Grantees by privilege set: `grantee → privileges` turned into `privileges → grantees`.
fn by_privileges(
    items: BTreeMap<String, BTreeSet<Privilege>>,
) -> BTreeMap<Vec<Privilege>, Vec<String>> {
    let mut out: BTreeMap<Vec<Privilege>, Vec<String>> = BTreeMap::new();
    for (grantee, privileges) in items {
        out.entry(privileges.into_iter().collect())
            .or_default()
            .push(grantee);
    }
    out
}

impl Encoder<'_> {
    fn targets<'s>(&self, schema: &'s Schema) -> Vec<Target<'s>> {
        let s = schema.name.as_str();
        let default = |kind| object_default_acl(self.settings, kind);
        let view_columns = |columns: &'s [ViewColumn]| {
            columns
                .iter()
                .map(|c| (c.name.as_str(), &c.acl))
                .collect::<Vec<_>>()
        };
        let mut out = Vec::new();
        if self.schema_blocks.contains(s) {
            out.push(Target {
                address: Address::new("schema", vec![s.to_owned()]),
                acl: &schema.acl,
                default: schema_defaults(self.settings, s).1,
                columns: Vec::new(),
            });
        }
        for e in schema.enums.values() {
            out.push(Target {
                address: self.address("enum", s, &e.name),
                acl: &e.acl,
                default: default(ObjectKind::Enum),
                columns: Vec::new(),
            });
        }
        for c in schema.composites.values() {
            out.push(Target {
                address: self.address("composite", s, &c.name),
                acl: &c.acl,
                default: default(ObjectKind::Composite),
                columns: Vec::new(),
            });
        }
        for d in schema.domains.values() {
            out.push(Target {
                address: self.address("domain", s, &d.name),
                acl: &d.acl,
                default: default(ObjectKind::Domain),
                columns: Vec::new(),
            });
        }
        let serials = self.serial_sequences(schema);
        for q in schema.sequences.values() {
            if !serials.contains(&q.name) {
                out.push(Target {
                    address: self.address("sequence", s, &q.name),
                    acl: &q.acl,
                    default: default(ObjectKind::Sequence),
                    columns: Vec::new(),
                });
            }
        }
        for t in schema.tables.values() {
            out.push(Target {
                address: self.address("table", s, &t.name),
                acl: &t.acl,
                default: default(ObjectKind::Table),
                columns: t
                    .columns
                    .iter()
                    .map(|c| (c.name.as_str(), &c.acl))
                    .collect(),
            });
        }
        for v in schema.views.values() {
            out.push(Target {
                address: self.address("view", s, &v.name),
                acl: &v.acl,
                default: default(ObjectKind::View),
                columns: view_columns(&v.columns),
            });
        }
        for m in schema.materialized_views.values() {
            out.push(Target {
                address: self.address("materialized_view", s, &m.name),
                acl: &m.acl,
                default: default(ObjectKind::MaterializedView),
                columns: view_columns(&m.columns),
            });
        }
        for (kind, routines, labels, object_kind) in [
            (
                "function",
                &schema.functions,
                &self.function_labels,
                ObjectKind::Function,
            ),
            (
                "procedure",
                &schema.procedures,
                &self.procedure_labels,
                ObjectKind::Procedure,
            ),
        ] {
            for (signature, f) in routines {
                let Some(label) = labels.get(&(s.to_owned(), signature.clone())) else {
                    continue;
                };
                out.push(Target {
                    address: self.address(kind, s, label),
                    acl: &f.acl,
                    default: default(object_kind),
                    columns: Vec::new(),
                });
            }
        }
        out
    }

    /// `grant` and `revoke` blocks for the objects of `schema`, objects with the same changes
    /// grouped into one block.
    pub(super) fn grants(&self, schema: &Schema) -> Vec<WBlock> {
        let mut grants: BTreeMap<GrantKey, Vec<Address>> = BTreeMap::new();
        let mut revokes: BTreeMap<RevokeKey, Vec<Address>> = BTreeMap::new();
        for target in self.targets(schema) {
            for grantable in [false, true] {
                let mut added: BTreeMap<String, BTreeSet<Privilege>> = BTreeMap::new();
                for item in target.acl {
                    let in_default = target.default.contains(item);
                    if item.grantable == grantable && !in_default {
                        added
                            .entry(item.grantee.clone())
                            .or_default()
                            .insert(item.privilege);
                    }
                }
                for (privileges, grantees) in by_privileges(added) {
                    grants
                        .entry((privileges, Vec::new(), grantees, grantable))
                        .or_default()
                        .push(target.address.clone());
                }
                let mut columns: BTreeMap<(String, Vec<Privilege>), Vec<String>> = BTreeMap::new();
                for (column, acl) in &target.columns {
                    let mut per_grantee: BTreeMap<String, BTreeSet<Privilege>> = BTreeMap::new();
                    for item in acl.iter().filter(|i| i.grantable == grantable) {
                        per_grantee
                            .entry(item.grantee.clone())
                            .or_default()
                            .insert(item.privilege);
                    }
                    for (grantee, privileges) in per_grantee {
                        columns
                            .entry((grantee, privileges.into_iter().collect()))
                            .or_default()
                            .push((*column).to_owned());
                    }
                }
                let mut grouped: BTreeMap<(Vec<Privilege>, Vec<String>), Vec<String>> =
                    BTreeMap::new();
                for ((grantee, privileges), cols) in columns {
                    grouped.entry((privileges, cols)).or_default().push(grantee);
                }
                for ((privileges, cols), grantees) in grouped {
                    grants
                        .entry((privileges, cols, grantees, grantable))
                        .or_default()
                        .push(target.address.clone());
                }
            }
            let mut removed: BTreeMap<String, BTreeSet<Privilege>> = BTreeMap::new();
            for item in &target.default {
                let kept = target
                    .acl
                    .iter()
                    .any(|i| i.grantee == item.grantee && i.privilege == item.privilege);
                if !kept {
                    removed
                        .entry(item.grantee.clone())
                        .or_default()
                        .insert(item.privilege);
                }
            }
            for (privileges, grantees) in by_privileges(removed) {
                revokes
                    .entry((privileges, grantees))
                    .or_default()
                    .push(target.address.clone());
            }
        }
        let privileges = |ps: &[Privilege]| {
            WExpr::List(
                ps.iter()
                    .map(|p| ident(&p.as_sql().replace(' ', "_")))
                    .collect(),
            )
        };
        let roles =
            |names: &[String]| WExpr::List(names.iter().map(|r| self.role_expr(r)).collect());
        let mut out = Vec::new();
        for ((ps, grantees), on) in revokes {
            let mut b = WBlock::new("revoke", Vec::new());
            b.push_attr("privileges", privileges(&ps));
            b.push_attr("on", WExpr::List(on.into_iter().map(WExpr::Ref).collect()));
            b.push_attr("from", roles(&grantees));
            out.push(b);
        }
        for ((ps, columns, grantees, grantable), on) in grants {
            let mut b = WBlock::new("grant", Vec::new());
            b.push_attr("privileges", privileges(&ps));
            b.push_attr("on", WExpr::List(on.into_iter().map(WExpr::Ref).collect()));
            if !columns.is_empty() {
                b.push_attr(
                    "columns",
                    WExpr::List(columns.into_iter().map(WExpr::String).collect()),
                );
            }
            b.push_attr("to", roles(&grantees));
            if grantable {
                b.push_attr("with_grant_option", WExpr::Bool(true));
            }
            out.push(b);
        }
        out
    }
}
