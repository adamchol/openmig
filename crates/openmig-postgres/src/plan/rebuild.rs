//! Dependants that block a change: found in the current dependency graph, dropped before the
//! change and created again (from the desired model) after it.

use super::refs::type_refs;
use super::step::{Phase, Step, rank};
use super::{Planner, is_nested_in, nested};
use crate::diff::{AlterOp, Change, MANUAL_HINT};
use crate::model::Object;
use crate::sql::{self, noun, relation_of};
use openmig_core::{Error, ObjectKind, ObjectRef};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// How a blocking dependant is taken out of the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rebuild {
    /// Drop it and create it again.
    Full,
    /// Drop the default of a column or domain and set it again.
    Default,
    /// Drop the column and add it again: generated columns (their values are recomputed) and,
    /// with `allow_recreate`, columns whose type is recreated (their values are lost).
    Column { destructive: bool },
}

impl Rebuild {
    /// Whether the object itself is dropped.
    pub fn removes_object(self) -> bool {
        !matches!(self, Rebuild::Default)
    }
}

/// A pseudo object standing for the default of a column or domain, so that steps can be
/// ordered around its removal.
pub(crate) fn default_ref(r: &ObjectRef) -> ObjectRef {
    let mut path = r.path.clone();
    path.push(String::new());
    ObjectRef { kind: r.kind, path }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Blocking {
    /// The object is dropped for good: every dependant blocks, but one that is redefined in
    /// place no longer uses it once redefined.
    Dropped,
    /// The object is dropped and created again: every dependant blocks.
    Removed,
    /// A column's type or expression changes: PostgreSQL rebuilds indexes and constraints itself.
    TypeChange,
}

/// The dependants to rebuild for `changes`, by object.
pub(crate) fn expand(
    p: &Planner<'_>,
    changes: &[Change],
) -> Result<BTreeMap<ObjectRef, Rebuild>, Error> {
    let cur = p.cur;
    let mut dependants: BTreeMap<&ObjectRef, Vec<&ObjectRef>> = BTreeMap::new();
    for d in &cur.dependencies {
        dependants.entry(&d.to).or_default().push(&d.from);
    }
    let redefined: BTreeSet<&ObjectRef> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Alter { object, ops }
                if ops.contains(&AlterOp::Replace) || ops.contains(&AlterOp::PolicyDefinition) =>
            {
                Some(object)
            }
            _ => None,
        })
        .collect();
    let mut queue: VecDeque<(ObjectRef, Blocking)> = VecDeque::new();
    for c in changes {
        match c {
            Change::Drop(r) => {
                queue.push_back((r.clone(), Blocking::Dropped));
                queue.extend(nested(cur, r).into_iter().map(|n| (n, Blocking::Dropped)));
            }
            Change::Recreate { object: r, .. } => {
                queue.push_back((r.clone(), Blocking::Removed));
                queue.extend(nested(cur, r).into_iter().map(|n| (n, Blocking::Removed)));
            }
            Change::Alter { object, ops } => {
                let set_expression = ops.contains(&AlterOp::Generated)
                    && matches!(p.des.get(object), Some(Object::Column(c)) if c.generated.is_some());
                let attribute_type = ops.iter().any(|o| matches!(o, AlterOp::AlterAttribute(_)));
                if ops.contains(&AlterOp::Type) || set_expression || attribute_type {
                    queue.push_back((object.clone(), Blocking::TypeChange));
                }
            }
            _ => {}
        }
    }
    let mut out: BTreeMap<ObjectRef, Rebuild> = BTreeMap::new();
    while let Some((x, how)) = queue.pop_front() {
        let Some(ds) = dependants.get(&x) else {
            continue;
        };
        for d in ds.iter().copied() {
            if p.removed.contains(d) || (how == Blocking::Dropped && redefined.contains(d)) {
                continue;
            }
            if how == Blocking::TypeChange
                && matches!(
                    d.kind,
                    ObjectKind::Index
                        | ObjectKind::PrimaryKey
                        | ObjectKind::Unique
                        | ObjectKind::Check
                        | ObjectKind::Exclusion
                        | ObjectKind::ForeignKey
                )
            {
                continue;
            }
            let Some((target, kind)) = classify(p, d, &x)? else {
                continue;
            };
            if out.contains_key(&target) || p.removed.contains(&target) || !p.des.contains(&target)
            {
                continue;
            }
            out.insert(target.clone(), kind);
            if kind.removes_object() {
                queue.push_back((target.clone(), Blocking::Removed));
                queue.extend(
                    nested(cur, &target)
                        .into_iter()
                        .map(|n| (n, Blocking::Removed)),
                );
            }
        }
    }
    let containers: Vec<ObjectRef> = out
        .iter()
        .filter(|(_, k)| k.removes_object())
        .map(|(r, _)| r.clone())
        .collect();
    out.retain(|r, _| !containers.iter().any(|c| is_nested_in(r, c)));
    Ok(out)
}

/// What to rebuild for dependant `d` of `x`, if anything.
fn classify(
    p: &Planner<'_>,
    d: &ObjectRef,
    x: &ObjectRef,
) -> Result<Option<(ObjectRef, Rebuild)>, Error> {
    let cur = p.cur;
    let relation = |kind: ObjectKind| ObjectRef::new(kind, [&d.path[0], &d.path[1]]);
    let typed_by = |data_type: &str| type_refs(cur, data_type).contains(x);
    Ok(Some(match (d.kind, cur.get(d)) {
        (ObjectKind::Column, Some(Object::Column(c))) => {
            if c.generated.is_some() {
                (d.clone(), Rebuild::Column { destructive: false })
            } else if typed_by(&c.data_type) {
                if !p.opts.allow_recreate {
                    return Err(Error::Unsupported {
                        object: d.clone(),
                        reason: format!("its type {} has to be recreated", x.dotted()),
                        hint: Some(MANUAL_HINT.to_owned()),
                    });
                }
                (d.clone(), Rebuild::Column { destructive: true })
            } else if c.default.is_some() {
                (d.clone(), Rebuild::Default)
            } else {
                return Ok(None);
            }
        }
        (ObjectKind::Column, Some(Object::ViewColumn(_))) => {
            let view = relation(ObjectKind::View);
            if cur.contains(&view) {
                (view, Rebuild::Full)
            } else {
                (relation(ObjectKind::MaterializedView), Rebuild::Full)
            }
        }
        (ObjectKind::Column, Some(Object::CompositeField(_))) => {
            (relation(ObjectKind::Composite), Rebuild::Full)
        }
        (ObjectKind::Domain, Some(Object::Domain(dom))) => {
            if typed_by(&dom.data_type) {
                (d.clone(), Rebuild::Full)
            } else {
                (d.clone(), Rebuild::Default)
            }
        }
        (ObjectKind::Table, _) => {
            return Err(Error::Unsupported {
                object: d.clone(),
                reason: format!("it depends on {x}, which has to be dropped"),
                hint: Some(MANUAL_HINT.to_owned()),
            });
        }
        (
            ObjectKind::Sequence | ObjectKind::Schema | ObjectKind::Extension | ObjectKind::Role,
            _,
        ) => return Ok(None),
        (_, Some(_)) => (d.clone(), Rebuild::Full),
        (_, None) => return Ok(None),
    }))
}

/// Remove changes that rebuilds make redundant: in-place changes of rebuilt objects, changes of
/// objects nested in them, and default changes of rebuilt defaults.
pub(crate) fn supersede(
    changes: Vec<Change>,
    rebuilds: &BTreeMap<ObjectRef, Rebuild>,
) -> Vec<Change> {
    let containers: Vec<&ObjectRef> = rebuilds
        .iter()
        .filter(|(_, k)| k.removes_object())
        .map(|(r, _)| r)
        .collect();
    changes
        .into_iter()
        .filter_map(|c| {
            let object = c.object_ref();
            if containers.iter().any(|r| is_nested_in(&object, r)) {
                return None;
            }
            match (c, rebuilds.get(&object)) {
                (Change::Alter { .. }, Some(k)) if k.removes_object() => None,
                (Change::Alter { object, ops }, Some(Rebuild::Default)) => {
                    let ops: Vec<AlterOp> =
                        ops.into_iter().filter(|o| *o != AlterOp::Default).collect();
                    (!ops.is_empty()).then_some(Change::Alter { object, ops })
                }
                (c, _) => Some(c),
            }
        })
        .collect()
}

impl Planner<'_> {
    pub(super) fn rebuild(&mut self, r: &ObjectRef, how: Rebuild) -> Result<(), Error> {
        match how {
            Rebuild::Full => {
                self.drop(r)?;
                self.create(r)
            }
            Rebuild::Column { destructive } => {
                let first = self.steps.len();
                self.drop(r)?;
                if !destructive {
                    self.keep_data(first);
                }
                self.create(r)
            }
            Rebuild::Default => {
                self.rebuild_default(r);
                Ok(())
            }
        }
    }

    fn rebuild_default(&mut self, r: &ObjectRef) {
        let default = match self.des.get(r) {
            Some(Object::Column(c)) => c.default.clone(),
            Some(Object::Domain(d)) => d.default.clone(),
            _ => return,
        };
        let statement = |action: &str| match r.kind {
            ObjectKind::Column => sql::alter_column(&relation_of(r), r.name(), action),
            _ => sql::alter_domain(&r.path[0], &r.path[1], action),
        };
        let mut drop = Step::new(r, Phase::Create, rank::COLUMN, " drop default");
        drop.sql(
            statement("DROP DEFAULT"),
            format!("Drop the default of {}", noun(r)),
        );
        drop.releases.push(r.clone());
        drop.removes.push(default_ref(r));
        self.push(drop);
        if let Some(default) = default {
            let mut set = Step::new(r, Phase::Create, rank::COLUMN, " set default");
            set.sql(
                statement(&format!("SET DEFAULT {default}")),
                format!("Set the default of {}", noun(r)),
            );
            set.requires.push(r.clone());
            set.requires.extend(self.des_deps(r));
            set.after_removal_of.push(default_ref(r));
            set.hints.push(default);
            self.push(set);
        }
    }
}
