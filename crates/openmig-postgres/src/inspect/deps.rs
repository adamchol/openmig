use super::{Catalog, SystemCatalog};
use crate::model::{Database, Dependency};
use openmig_core::{ObjectKind, ObjectRef};
use tokio_postgres::Row;

pub(super) struct Rows<'a> {
    pub derived: &'a [Row],
    pub parts: &'a [Row],
    pub depends: &'a [Row],
    pub members: &'a [Row],
    pub policy_roles: &'a [Row],
}

/// Turn `pg_depend` (and policy role references from `pg_shdepend`) into model dependencies.
pub(super) fn dependencies(db: &mut Database, cat: &mut Catalog, rows: Rows<'_>) {
    for r in rows.derived {
        cat.derived_types
            .insert(r.get("oid"), (r.get("elem"), r.get("rel")));
    }
    for r in rows.parts {
        let class = SystemCatalog::parse(r.get("class"));
        cat.relation_parts
            .insert((class, r.get("oid")), (r.get("rel"), r.get("num")));
    }
    for r in rows.members {
        let class = SystemCatalog::parse(r.get("class"));
        cat.extension_members
            .insert((class, r.get("objid")), r.get("extension"));
    }
    let mut out = Vec::new();
    for r in rows.depends {
        let class = SystemCatalog::parse(r.get("class"));
        let from = resolve(cat, class, r.get("objid"), r.get("objsubid"), 0);
        let to = resolve(
            cat,
            SystemCatalog::parse(r.get("refclass")),
            r.get("refobjid"),
            r.get("refobjsubid"),
            0,
        );
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        // Objects inside an extension resolve to it; only the extension's own dependencies count.
        let inside_extension =
            from.kind == ObjectKind::Extension && class != SystemCatalog::Extension;
        // `OWNED BY` is recorded in `Sequence::owned_by`; as an edge it would form a cycle with
        // the column default that calls `nextval`.
        let owned_by = from.kind == ObjectKind::Sequence && to.kind == ObjectKind::Column;
        if inside_extension || owned_by || from == to || contains(&to, &from) {
            continue;
        }
        out.push(Dependency { from, to });
    }
    for r in rows.policy_roles {
        let role: String = r.get("role");
        if let Some(policy) = cat.objects.get(&(SystemCatalog::Policy, r.get("policy")))
            && db.roles.contains_key(&role)
        {
            out.push(Dependency {
                from: policy.clone(),
                to: ObjectRef::new(ObjectKind::Role, [role]),
            });
        }
    }
    db.dependencies = out;
    db.normalize_dependencies();
}

/// Whether `outer` is a container of `inner` (a schema of its objects, a relation of its columns
/// and constraints, a domain of its checks).
fn contains(outer: &ObjectRef, inner: &ObjectRef) -> bool {
    matches!(
        outer.kind,
        ObjectKind::Schema
            | ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Domain
            | ObjectKind::Composite
    ) && inner.path.len() > outer.path.len()
        && inner.path.starts_with(&outer.path)
}

/// The model object a catalog object corresponds to: the object itself, its column, or for
/// derived objects (rules, defaults, array and row types) the object they belong to. Objects that
/// belong to an extension resolve to the extension.
fn resolve(
    cat: &Catalog,
    class: SystemCatalog,
    oid: u32,
    sub: i32,
    depth: u8,
) -> Option<ObjectRef> {
    if let Some(extension) = cat.extension_members.get(&(class, oid)) {
        return Some(ObjectRef::new(ObjectKind::Extension, [extension]));
    }
    if depth > 8 {
        return None;
    }
    match class {
        SystemCatalog::Class => {
            if let Some(r) = cat.objects.get(&(SystemCatalog::Class, oid)) {
                return Some(r.clone());
            }
            let rel = cat.relations.get(&oid)?;
            if sub > 0 && rel.kind != super::RelKind::Composite {
                let column = cat.columns.get(&(oid, i16::try_from(sub).ok()?))?;
                return Some(ObjectRef::new(
                    ObjectKind::Column,
                    [&rel.schema, &rel.name, column],
                ));
            }
            rel.object_ref()
        }
        SystemCatalog::Type => {
            if let Some(r) = cat.objects.get(&(SystemCatalog::Type, oid)) {
                return Some(r.clone());
            }
            let (elem, rel) = *cat.derived_types.get(&oid)?;
            if rel != 0 {
                resolve(cat, SystemCatalog::Class, rel, 0, depth + 1)
            } else {
                resolve(cat, SystemCatalog::Type, elem, 0, depth + 1)
            }
        }
        SystemCatalog::Rewrite | SystemCatalog::AttrDef => {
            let (rel, num) = *cat.relation_parts.get(&(class, oid))?;
            resolve(cat, SystemCatalog::Class, rel, i32::from(num), depth + 1)
        }
        SystemCatalog::Namespace => cat
            .namespaces
            .get(&oid)
            .map(|n| ObjectRef::new(ObjectKind::Schema, [n])),
        SystemCatalog::Other => None,
        _ => cat.objects.get(&(class, oid)).cloned(),
    }
}
