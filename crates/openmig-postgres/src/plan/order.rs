//! Ordering steps with a dependency graph.
//!
//! Edges, all from "must run first" to "runs later":
//! * a step creating X before steps requiring X;
//! * a step releasing X (dropping or redefining it) before steps removing what X depends on in
//!   the current model (dependants are dropped first);
//! * a step removing X before a step creating an object that takes the same name;
//! * removals of objects before the drop of the schema or relation containing them;
//! * explicit `after_removal_of` / `before_removal_of` constraints;
//! * renames before every other step on the renamed object;
//! * every other change of a table before its rows are written; rows of referenced tables are
//!   inserted first and deleted last;
//! * revokes before the grantee role is dropped;
//! * soft edges from identifiers in SQL text (scratch builds, function bodies), dropped when they
//!   would form a cycle.

use super::refs::NameIndex;
use super::step::{Step, StepKind};
use super::{is_nested_in, refs::type_refs};
use crate::diff::Options;
use crate::model::Database;
use crate::sql;
use openmig_core::graph::{DepGraph, NodeId};
use openmig_core::{Error, ObjectKind, ObjectRef, PlanMode, TxRequirement};
use std::collections::BTreeMap;

/// A name that two objects cannot share at the same time.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    /// Relations (tables, views, sequences, indexes, composite types) share a namespace.
    Relation(String, String),
    /// So do types (including the row types of relations).
    Type(String, String),
    Constraint(String, String, String),
    /// A table has at most one primary key.
    PrimaryKeyOf(String, String),
    Routine(String, String),
    Exact(ObjectRef),
}

fn slots(r: &ObjectRef) -> Vec<Slot> {
    use ObjectKind as K;
    let p = &r.path;
    match (r.kind, p.as_slice()) {
        (K::Table | K::View | K::MaterializedView | K::Composite, [s, n]) => vec![
            Slot::Relation(s.clone(), n.clone()),
            Slot::Type(s.clone(), n.clone()),
        ],
        (K::Sequence, [s, n]) => vec![Slot::Relation(s.clone(), n.clone())],
        (K::Enum | K::Domain, [s, n]) => vec![Slot::Type(s.clone(), n.clone())],
        (K::Function | K::Procedure, [s, sig]) => vec![Slot::Routine(s.clone(), sig.clone())],
        (K::Index, [s, _, i]) => vec![Slot::Relation(s.clone(), i.clone())],
        (K::PrimaryKey, [s, t, c]) => vec![
            Slot::Relation(s.clone(), c.clone()),
            Slot::Constraint(s.clone(), t.clone(), c.clone()),
            Slot::PrimaryKeyOf(s.clone(), t.clone()),
        ],
        (K::Unique | K::Exclusion, [s, t, c]) => vec![
            Slot::Relation(s.clone(), c.clone()),
            Slot::Constraint(s.clone(), t.clone(), c.clone()),
        ],
        (K::Check | K::ForeignKey, [s, t, c]) => {
            vec![Slot::Constraint(s.clone(), t.clone(), c.clone())]
        }
        _ => vec![Slot::Exact(r.clone())],
    }
}

fn index<'s, F>(steps: &'s [Step], field: F) -> BTreeMap<&'s ObjectRef, Vec<usize>>
where
    F: Fn(&'s Step) -> &'s Vec<ObjectRef>,
{
    let mut out: BTreeMap<&ObjectRef, Vec<usize>> = BTreeMap::new();
    for (i, s) in steps.iter().enumerate() {
        for r in field(s) {
            out.entry(r).or_default().push(i);
        }
    }
    out
}

/// Sort `steps`; `cur` is the (re-keyed) current model, `des` the desired one.
pub(super) fn sort(
    steps: Vec<Step>,
    cur: &Database,
    des: &Database,
    opts: &Options<'_>,
) -> Result<Vec<Step>, Error> {
    let mut graph: DepGraph<usize> = DepGraph::new();
    let ids: Vec<NodeId> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| graph.add(i, s.priority()))
        .collect();
    let edge = |graph: &mut DepGraph<usize>, a: usize, b: usize| {
        if a != b {
            graph.edge(ids[a], ids[b]);
        }
    };
    let creators = index(&steps, |s| &s.creates);
    let removers = index(&steps, |s| &s.removes);

    for (i, s) in steps.iter().enumerate() {
        for r in &s.requires {
            for &p in creators.get(r).into_iter().flatten() {
                edge(&mut graph, p, i);
            }
        }
    }

    let mut depends_on: BTreeMap<&ObjectRef, Vec<&ObjectRef>> = BTreeMap::new();
    for d in &cur.dependencies {
        depends_on.entry(&d.from).or_default().push(&d.to);
    }
    for (i, s) in steps.iter().enumerate() {
        for x in &s.releases {
            for y in depends_on.get(x).into_iter().flatten() {
                for &d in removers.get(*y).into_iter().flatten() {
                    edge(&mut graph, i, d);
                }
            }
        }
    }

    container_edges(&steps, &mut graph, &ids);

    let mut slot_removers: BTreeMap<Slot, Vec<usize>> = BTreeMap::new();
    for (i, s) in steps.iter().enumerate() {
        if s.kind == StepKind::Rename {
            continue;
        }
        for r in &s.removes {
            for slot in slots(r) {
                slot_removers.entry(slot).or_default().push(i);
            }
        }
    }
    for (i, s) in steps.iter().enumerate() {
        if s.kind == StepKind::Rename {
            continue;
        }
        for r in &s.creates {
            for slot in slots(r) {
                for &d in slot_removers.get(&slot).into_iter().flatten() {
                    edge(&mut graph, d, i);
                }
            }
        }
    }

    for (i, s) in steps.iter().enumerate() {
        for x in &s.after_removal_of {
            for &d in removers.get(x).into_iter().flatten() {
                edge(&mut graph, d, i);
            }
        }
        for x in &s.before_removal_of {
            for &d in removers.get(x).into_iter().flatten() {
                edge(&mut graph, i, d);
            }
        }
    }

    for (r, rename) in steps.iter().enumerate() {
        if rename.kind != StepKind::Rename {
            continue;
        }
        let to = &rename.subject;
        for (i, s) in steps.iter().enumerate() {
            if s.kind != StepKind::Rename
                && (s.subject.path == to.path || is_nested_in(&s.subject, to))
            {
                edge(&mut graph, r, i);
            }
        }
    }

    data_edges(&steps, cur, des, &mut graph, &ids);

    let drops_role: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.subject.kind == ObjectKind::Role && !s.removes.is_empty())
        .map(|(i, s)| (s.subject.name(), i))
        .collect();
    for (i, s) in steps.iter().enumerate() {
        for role in &s.revokes_from {
            if let Some(&d) = drops_role.get(role.as_str()) {
                edge(&mut graph, i, d);
            }
        }
    }

    let search_path: Vec<String> = opts
        .settings
        .search_path
        .iter()
        .filter(|s| s.as_str() != "$user")
        .cloned()
        .collect();
    let names = NameIndex::new(des, &search_path);
    let routine = |s: &Step| matches!(s.subject.kind, ObjectKind::Function | ObjectKind::Procedure);
    // Expressions are checked when their object is created; function bodies are not (with
    // `check_function_bodies = off`), so their guesses go last and lose conflicts.
    for bodies in [false, true] {
        for (i, s) in steps.iter().enumerate() {
            if routine(s) != bodies
                || s.kind == StepKind::EnumValues
                || !(opts.mode == PlanMode::Scratch || bodies)
            {
                continue;
            }
            for text in &s.hints {
                for r in names.lookup(text) {
                    for &p in creators.get(&r).into_iter().flatten() {
                        if p != i {
                            graph.soft_edge(ids[p], ids[i]);
                        }
                    }
                }
            }
        }
    }

    let order = graph.sort().map_err(|cycle| {
        let members: Vec<String> = cycle
            .items
            .iter()
            .map(|&i| {
                steps[i]
                    .statements
                    .first()
                    .map_or_else(|| steps[i].key.clone(), |s| s.comment.clone())
            })
            .collect();
        Error::other(format!(
            "cannot order the plan, these changes depend on each other: {}",
            members.join("; ")
        ))
    })?;
    let mut slots: Vec<Option<Step>> = steps.into_iter().map(Some).collect();
    Ok(order.into_iter().filter_map(|i| slots[i].take()).collect())
}

/// Objects are removed before the schema or relation that contains them is dropped. The model's
/// dependencies leave out these container edges.
fn container_edges(steps: &[Step], graph: &mut DepGraph<usize>, ids: &[NodeId]) {
    let mut schemas: BTreeMap<&str, usize> = BTreeMap::new();
    let mut relations: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (i, s) in steps.iter().enumerate() {
        for r in &s.removes {
            match (r.kind, r.path.as_slice()) {
                (ObjectKind::Schema, [n]) => {
                    schemas.insert(n, i);
                }
                (
                    ObjectKind::Table
                    | ObjectKind::View
                    | ObjectKind::MaterializedView
                    | ObjectKind::Composite
                    | ObjectKind::Domain,
                    [sc, n],
                ) => {
                    relations.insert((sc, n), i);
                }
                _ => {}
            }
        }
    }
    for (i, s) in steps.iter().enumerate() {
        for r in &s.removes {
            let containers = match r.path.as_slice() {
                [sc, _] => [schemas.get(sc.as_str()), None],
                [sc, rel, _] => [
                    schemas.get(sc.as_str()),
                    relations.get(&(sc.as_str(), rel.as_str())),
                ],
                _ => [None, None],
            };
            for &d in containers.into_iter().flatten() {
                if d != i {
                    graph.edge(ids[i], ids[d]);
                }
            }
        }
    }
}

/// Rows are written after every other change of their table. Rows of a referenced table are
/// inserted and updated before those of the referencing table, and deleted after the referencing
/// table's rows are deleted or updated. These are soft edges: foreign keys between data tables
/// may form cycles.
fn data_edges(
    steps: &[Step],
    cur: &Database,
    des: &Database,
    graph: &mut DepGraph<usize>,
    ids: &[NodeId],
) {
    let data: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == StepKind::Data)
        .map(|(i, _)| i)
        .collect();
    for &d in &data {
        let table = &steps[d].subject.path;
        for (i, s) in steps.iter().enumerate() {
            if s.kind != StepKind::Data && s.subject.path.starts_with(table) {
                graph.edge(ids[i], ids[d]);
            }
        }
    }
    let is_delete = |i: usize| steps[i].key.ends_with(" delete");
    let table_of = |i: usize| {
        crate::model::QualifiedName::new(&steps[i].subject.path[0], &steps[i].subject.path[1])
    };
    for &a in &data {
        for &b in &data {
            if a == b {
                continue;
            }
            let (ta, tb) = (table_of(a), table_of(b));
            if ta == tb {
                continue;
            }
            let references = |db: &Database, from: &crate::model::QualifiedName, to| {
                db.table(from)
                    .is_some_and(|t| t.foreign_keys.values().any(|fk| &fk.ref_table == to))
            };
            match (is_delete(a), is_delete(b)) {
                (false, false) if references(des, &tb, &ta) => {
                    graph.soft_edge(ids[a], ids[b]);
                }
                (true, true) if references(cur, &ta, &tb) => {
                    graph.soft_edge(ids[a], ids[b]);
                }
                (false, true) if references(des, &ta, &tb) => {
                    graph.soft_edge(ids[a], ids[b]);
                }
                _ => {}
            }
        }
    }
    for &a in &data {
        for &b in &data {
            if a != b && table_of(a) == table_of(b) && is_delete(a) && !is_delete(b) {
                graph.soft_edge(ids[a], ids[b]);
            }
        }
    }
}

/// An enum value added in a transaction cannot be used before the transaction commits. When a
/// later statement may use a value added by the plan, the `ADD VALUE` statements run outside a
/// transaction (and so does the whole migration).
pub(super) fn forbid_enum_values_in_use(steps: &mut [Step], desired: &Database) {
    for e in 0..steps.len() {
        if steps[e].kind != StepKind::EnumValues {
            continue;
        }
        let ty = steps[e].subject.clone();
        let values: Vec<String> = steps[e]
            .statements
            .iter()
            .filter_map(|s| added_value(&s.sql))
            .collect();
        let names = [
            format!("{}.{}", ty.path[0], ty.path[1]),
            sql::qualified(&ty.path[0], &ty.path[1]),
        ];
        let used = steps[e + 1..].iter().any(|s| {
            let related = s.requires.contains(&ty)
                || s.statements
                    .iter()
                    .any(|st| names.iter().any(|n| st.sql.contains(n.as_str())))
                || (s.kind == StepKind::Data && table_uses_type(desired, &s.subject, &ty));
            related
                && s.statements
                    .iter()
                    .any(|st| values.iter().any(|v| st.sql.contains(&sql::literal(v))))
        });
        if used {
            for st in &mut steps[e].statements {
                st.tx = TxRequirement::Forbidden;
            }
        }
    }
}

/// The label of an `ALTER TYPE ... ADD VALUE '...'` statement.
fn added_value(statement: &str) -> Option<String> {
    let rest = statement.split_once(" ADD VALUE '")?.1;
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                out.push('\'');
            } else {
                return Some(out);
            }
        } else {
            out.push(c);
        }
    }
    None
}

fn table_uses_type(db: &Database, data: &ObjectRef, ty: &ObjectRef) -> bool {
    let name = crate::model::QualifiedName::new(&data.path[0], &data.path[1]);
    db.table(&name).is_some_and(|t| {
        t.columns
            .iter()
            .any(|c| type_refs(db, &c.data_type).contains(ty))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_value_statements() {
        assert_eq!(
            added_value("ALTER TYPE \"a\".\"b\" ADD VALUE 'it''s' AFTER 'x'").as_deref(),
            Some("it's")
        );
        assert_eq!(added_value("SELECT 1"), None);
    }

    #[test]
    fn name_slots() {
        let t = ObjectRef::new(ObjectKind::Table, ["s", "x"]);
        let i = ObjectRef::new(ObjectKind::Index, ["s", "t", "x"]);
        let e = ObjectRef::new(ObjectKind::Enum, ["s", "x"]);
        let shared = |a: &ObjectRef, b: &ObjectRef| slots(a).iter().any(|s| slots(b).contains(s));
        assert!(shared(&t, &i));
        assert!(shared(&t, &e));
        assert!(!shared(&i, &e));
    }
}
