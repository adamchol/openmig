//! Sequences, enums, composite types and domains.

use super::{AlterOp, Differ, EnumPosition, merge};
use crate::model::{Composite, Database, Object, Schema};
use openmig_core::{ObjectKind, ObjectRef};
use std::collections::BTreeSet;

/// `ADD VALUE` operations that turn `cur` into `des`, or `None` when values were removed or
/// reordered. Values are added in desired order, each after its predecessor (or before the first
/// existing value), so the final order is the desired one.
pub(crate) fn enum_additions(
    cur: &[String],
    des: &[String],
) -> Option<Vec<(String, EnumPosition)>> {
    let mut rest = des.iter();
    for c in cur {
        if !rest.any(|d| d == c) {
            return None;
        }
    }
    let existing: BTreeSet<&String> = cur.iter().collect();
    let mut out = Vec::new();
    for (i, v) in des.iter().enumerate() {
        if existing.contains(v) {
            continue;
        }
        let position = match (i, cur.first()) {
            (0, Some(first)) => EnumPosition::Before(first.clone()),
            (0, None) => EnumPosition::End,
            _ => EnumPosition::After(des[i - 1].clone()),
        };
        out.push((v.clone(), position));
    }
    Some(out)
}

/// Whether a column of a table has the type `ty` (directly, as an array, or through domains and
/// composite types built on it). Such types cannot be dropped without losing data.
pub(crate) fn used_by_table_columns(db: &Database, ty: &ObjectRef) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![ty.clone()];
    while let Some(t) = stack.pop() {
        if !seen.insert(t.clone()) {
            continue;
        }
        for d in db.dependants_of(&t) {
            match d.kind {
                ObjectKind::Column if matches!(db.get(d), Some(Object::Column(_))) => {
                    return true;
                }
                ObjectKind::Domain | ObjectKind::Composite => stack.push(d.clone()),
                ObjectKind::Column if matches!(db.get(d), Some(Object::CompositeField(_))) => {
                    stack.push(ObjectRef::new(
                        ObjectKind::Composite,
                        [&d.path[0], &d.path[1]],
                    ));
                }
                _ => {}
            }
        }
    }
    false
}

/// Attribute changes that turn composite `cur` into `des` in place, or `None` when attributes
/// would end up in another order (`ADD ATTRIBUTE` appends).
fn composite_ops(cur: &Composite, des: &Composite) -> Option<Vec<AlterOp>> {
    let kept: Vec<&str> = cur
        .fields
        .iter()
        .filter(|c| des.fields.iter().any(|d| d.name == c.name))
        .map(|c| c.name.as_str())
        .collect();
    let added: Vec<&str> = des
        .fields
        .iter()
        .filter(|d| !cur.fields.iter().any(|c| c.name == d.name))
        .map(|d| d.name.as_str())
        .collect();
    let order: Vec<&str> = kept.iter().chain(&added).copied().collect();
    let desired: Vec<&str> = des.fields.iter().map(|d| d.name.as_str()).collect();
    if order != desired {
        return None;
    }
    let mut ops = Vec::new();
    for c in &cur.fields {
        match des.fields.iter().find(|d| d.name == c.name) {
            None => ops.push(AlterOp::DropAttribute(c.name.clone())),
            Some(d) if d != c => ops.push(AlterOp::AlterAttribute(c.name.clone())),
            Some(_) => {}
        }
    }
    ops.extend(
        added
            .into_iter()
            .map(|a| AlterOp::AddAttribute(a.to_owned())),
    );
    Some(ops)
}

impl Differ<'_> {
    /// Recreate a type whose definition cannot change in place. Without table columns using it
    /// nothing is lost; otherwise it needs `allow_recreate`.
    fn recreate_type(&mut self, r: ObjectRef, reason: &str) {
        if used_by_table_columns(self.cur, &r) {
            self.unsupported(r, format!("{reason} while table columns use the type"));
        } else {
            self.recreate(r, reason);
        }
    }

    pub(super) fn sequences(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.sequences, &des.sequences) {
            let r = ObjectRef::new(ObjectKind::Sequence, [s, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => {
                    let mut ops = Vec::new();
                    if c.data_type != d.data_type || c.options != d.options {
                        ops.push(AlterOp::SequenceOptions);
                    }
                    if c.owned_by != d.owned_by {
                        ops.push(AlterOp::OwnedBy);
                    }
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.acl_op(&mut ops, &c.acl, &d.acl);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    pub(super) fn enums(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.enums, &des.enums) {
            let r = ObjectRef::new(ObjectKind::Enum, [s, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => {
                    let Some(additions) = enum_additions(&c.values, &d.values) else {
                        self.unsupported(r, "enum values were removed or reordered");
                        continue;
                    };
                    let mut ops: Vec<AlterOp> = additions
                        .into_iter()
                        .map(|(value, position)| AlterOp::AddValue { value, position })
                        .collect();
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.acl_op(&mut ops, &c.acl, &d.acl);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    pub(super) fn composites(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.composites, &des.composites) {
            let r = ObjectRef::new(ObjectKind::Composite, [s, name]);
            match (c, d) {
                (None, Some(_)) => self.create(r),
                (Some(_), None) => self.drop(r),
                (Some(c), Some(d)) => {
                    let Some(mut ops) = composite_ops(c, d) else {
                        self.recreate_type(r, "the attributes of the type were reordered");
                        continue;
                    };
                    let alters_used = ops.iter().any(|o| matches!(o, AlterOp::AlterAttribute(_)))
                        && used_by_table_columns(self.cur, &r);
                    if alters_used {
                        self.unsupported(
                            r,
                            "the type of an attribute changed while table columns use the type",
                        );
                        continue;
                    }
                    Self::comment_op(&mut ops, &c.comment, &d.comment);
                    self.acl_op(&mut ops, &c.acl, &d.acl);
                    self.alter(r, ops);
                }
                (None, None) => {}
            }
        }
    }

    pub(super) fn domains(&mut self, cur: &Schema, des: &Schema) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        for (name, c, d) in merge(&cur.domains, &des.domains) {
            let r = ObjectRef::new(ObjectKind::Domain, [s, name]);
            let (c, d) = match (c, d) {
                (None, Some(_)) => {
                    self.create(r);
                    continue;
                }
                (Some(_), None) => {
                    self.drop(r);
                    continue;
                }
                (Some(c), Some(d)) => (c, d),
                (None, None) => continue,
            };
            if c.data_type != d.data_type || c.collation != d.collation {
                self.recreate_type(r, "the base type or collation of the domain changed");
                continue;
            }
            let mut ops = Vec::new();
            if c.default != d.default {
                ops.push(AlterOp::Default);
            }
            if c.not_null != d.not_null {
                ops.push(AlterOp::NotNull);
            }
            Self::comment_op(&mut ops, &c.comment, &d.comment);
            self.acl_op(&mut ops, &c.acl, &d.acl);
            self.alter(r, ops);
            for (check, cc, dc) in merge(&c.checks, &d.checks) {
                let cr = ObjectRef::new(ObjectKind::Check, [s, name, check]);
                self.constraint(
                    cr,
                    cc,
                    dc,
                    |x| {
                        x.comment = None;
                        x.validated = true;
                    },
                    |x| (x.validated, &x.comment),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CompositeField;

    fn v(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn enum_values() {
        assert_eq!(
            enum_additions(&v(&["b", "d"]), &v(&["a", "b", "c", "d", "e"])).unwrap(),
            vec![
                ("a".to_owned(), EnumPosition::Before("b".into())),
                ("c".to_owned(), EnumPosition::After("b".into())),
                ("e".to_owned(), EnumPosition::After("d".into())),
            ]
        );
        assert_eq!(
            enum_additions(&[], &v(&["a", "b"])).unwrap(),
            vec![
                ("a".to_owned(), EnumPosition::End),
                ("b".to_owned(), EnumPosition::After("a".into())),
            ]
        );
        assert!(enum_additions(&v(&["a", "b"]), &v(&["b", "a"])).is_none());
        assert!(enum_additions(&v(&["a", "b"]), &v(&["a"])).is_none());
        assert_eq!(enum_additions(&v(&["a"]), &v(&["a"])).unwrap(), vec![]);
    }

    fn field(name: &str, ty: &str) -> CompositeField {
        CompositeField {
            name: name.into(),
            data_type: ty.into(),
            collation: None,
        }
    }

    #[test]
    fn composite_attributes() {
        let cur = Composite {
            name: "c".into(),
            fields: vec![field("a", "int"), field("b", "text"), field("c", "text")],
            ..Composite::default()
        };
        let des = Composite {
            name: "c".into(),
            fields: vec![field("a", "bigint"), field("c", "text"), field("d", "text")],
            ..Composite::default()
        };
        assert_eq!(
            composite_ops(&cur, &des).unwrap(),
            vec![
                AlterOp::AlterAttribute("a".into()),
                AlterOp::DropAttribute("b".into()),
                AlterOp::AddAttribute("d".into()),
            ]
        );
        let reordered = Composite {
            name: "c".into(),
            fields: vec![field("d", "text"), field("a", "int")],
            ..Composite::default()
        };
        assert!(composite_ops(&cur, &reordered).is_none());
    }
}
