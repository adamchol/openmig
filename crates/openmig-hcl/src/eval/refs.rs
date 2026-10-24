//! Resolution of traversals: `var.*`, `local.*`, `each.*`, `for` bindings, block references
//! (`table.app.users.column.id`, relative `column.id`, `.name`) and indexing into values.

use super::collect::NodeId;
use super::expr::{Eval, Key, Scope, Segment};
use super::{Fail, Loader};
use crate::decode::suggest;
use crate::diag::Diagnostic;
use crate::source::Span;
use crate::spec::{BlockSpec, LabelSpec};
use crate::value::{Address, Value};

impl Loader<'_> {
    /// Resolves a traversal whose root is the bare identifier `root`.
    pub(super) fn resolve_path(
        &self,
        root: &str,
        root_span: Span,
        segments: &[Segment],
        scope: &Scope<'_>,
    ) -> Eval {
        if let Some(value) = scope.lookup(root) {
            return self.index_value(value.clone(), segments);
        }
        match root {
            "var" | "local" => return self.named_value(root, root_span, segments, scope),
            "each" => return self.each_value(root_span, segments, scope),
            _ => {}
        }
        if segments.is_empty() {
            return Ok(Value::Ident(root.to_owned()));
        }
        if let Some((anchor, spec)) = self.enclosing_kind(scope.block, root) {
            return self.relative_ref(anchor, spec, root_span, segments);
        }
        let kinds: &'static [BlockSpec] = self.spec.blocks;
        if let Some(spec) = kinds.iter().find(|b| b.kind == root) {
            return self.block_ref(spec, root_span, segments);
        }
        let mut candidates = vec!["var", "local"];
        if scope.each.is_some() {
            candidates.push("each");
        }
        candidates.extend(kinds.iter().filter(|b| b.referenceable).map(|b| b.kind));
        let mut d =
            Diagnostic::error(format!("unknown variable or block kind `{root}`")).at(root_span);
        if let Some(s) = suggest(root, candidates) {
            d = d.with_help(format!("did you mean `{s}`?"));
        }
        Err(self.error(d))
    }

    /// `var.<name>` and `local.<name>`.
    fn named_value(
        &self,
        root: &str,
        root_span: Span,
        segments: &[Segment],
        scope: &Scope<'_>,
    ) -> Eval {
        if scope.var_default {
            return Err(self.error(
                Diagnostic::error(format!("a variable default cannot use `{root}`")).at(root_span),
            ));
        }
        let Some((first, rest)) = segments.split_first() else {
            return Err(self.error(
                Diagnostic::error(format!("`{root}` must be followed by a name"))
                    .at(root_span)
                    .with_help(format!("write `{root}.<name>`")),
            ));
        };
        let Some(name) = first.key.as_label() else {
            return Err(self.error(
                Diagnostic::error(format!("expected a name after `{root}`")).at(first.span),
            ));
        };
        let at = root_span.to(first.span);
        let value = if root == "var" {
            match self.variables.iter().position(|v| v.name == name) {
                Some(i) => self.var_value(i, at)?,
                None => {
                    let names = self.variables.iter().map(|v| v.name.as_str());
                    return Err(self.unknown_name("variable", &name, names, at));
                }
            }
        } else {
            match self.locals.iter().position(|l| l.name == name) {
                Some(i) => self.local_value(i, at)?,
                None => {
                    let names = self.locals.iter().map(|l| l.name.as_str());
                    return Err(self.unknown_name("local value", &name, names, at));
                }
            }
        };
        self.index_value(value, rest)
    }

    fn unknown_name<'n>(
        &self,
        what: &str,
        name: &str,
        names: impl Iterator<Item = &'n str>,
        at: Span,
    ) -> Fail {
        let mut d = Diagnostic::error(format!("unknown {what} `{name}`")).at(at);
        if let Some(s) = suggest(name, names) {
            d = d.with_help(format!("did you mean `{s}`?"));
        }
        self.error(d)
    }

    fn each_value(&self, root_span: Span, segments: &[Segment], scope: &Scope<'_>) -> Eval {
        let Some(each) = scope.each else {
            return Err(self.error(
                Diagnostic::error("`each` is only available in blocks with `for_each`")
                    .at(root_span),
            ));
        };
        let value = match segments.first().map(|s| &s.key) {
            Some(Key::Name(name)) if name == "key" => &each.key,
            Some(Key::Name(name)) if name == "value" => &each.value,
            _ => {
                let at = segments.first().map_or(root_span, |s| s.span);
                return Err(self.error(
                    Diagnostic::error("`each` has only `each.key` and `each.value`").at(at),
                ));
            }
        };
        self.index_value(value.clone(), &segments[1..])
    }

    /// The innermost enclosing block that has `kind` as a referenceable nested kind.
    fn enclosing_kind(
        &self,
        block: Option<NodeId>,
        kind: &str,
    ) -> Option<(NodeId, &'static BlockSpec)> {
        let mut current = block;
        while let Some(id) = current {
            let node = &self.nodes[id];
            let nested: &'static [BlockSpec] = node.spec.nested;
            if let Some(spec) = nested.iter().find(|b| b.kind == kind && b.referenceable) {
                return Some((id, spec));
            }
            current = node.parent;
        }
        None
    }

    /// `column.id` inside a table.
    fn relative_ref(
        &self,
        anchor: NodeId,
        spec: &BlockSpec,
        root_span: Span,
        segments: &[Segment],
    ) -> Eval {
        let node = &self.nodes[anchor];
        let Some(address) = &node.address else {
            let label = segments[0].key.as_label().unwrap_or_default();
            return Err(self.error(
                Diagnostic::error(format!(
                    "cannot reference `{}.{label}` here: the enclosing `{}` block has no \
                     address",
                    spec.kind, node.spec.kind
                ))
                .at(root_span.to(segments[0].span))
                .with_help(format!(
                    "blocks created by `for_each` and unlabelled blocks cannot be referenced; \
                     use the name \"{label}\" as a string instead"
                )),
            ));
        };
        self.follow(
            address.clone(),
            anchor,
            root_span,
            segments,
            Some(spec.kind),
        )
    }

    /// `table.app.users...`, `role.web...`.
    fn block_ref(&self, spec: &'static BlockSpec, root_span: Span, segments: &[Segment]) -> Eval {
        let kind = spec.kind;
        if !spec.referenceable || spec.labels == LabelSpec::None {
            return Err(self.error(
                Diagnostic::error(format!("`{kind}` blocks cannot be referenced")).at(root_span),
            ));
        }
        let first = &segments[0];
        let Some(s1) = first.key.as_label() else {
            return Err(self.error(
                Diagnostic::error(format!("expected a label after `{kind}`")).at(first.span),
            ));
        };
        let s2 = segments.get(1).and_then(|s| s.key.as_label());
        let candidates = || {
            self.by_address
                .iter()
                .filter(|(a, _)| a.kind == kind && a.nested.is_empty())
        };
        let found = if spec.labels == LabelSpec::Qualified {
            let exact = s2.as_ref().and_then(|s2| {
                let address = Address::new(kind, vec![s1.clone(), s2.clone()]);
                self.by_address.get(&address).map(|&id| (address, id, 2))
            });
            match exact {
                Some(found) => Some(found),
                None => match self.by_name.get(&(kind, s1.clone())).map(Vec::as_slice) {
                    Some([id]) => self.nodes[*id].address.clone().map(|a| (a, *id, 1)),
                    Some(ids) if ids.len() > 1 => {
                        let list: Vec<String> = ids
                            .iter()
                            .filter_map(|id| self.nodes[*id].address.as_ref())
                            .map(Address::to_string)
                            .collect();
                        return Err(self.error(
                            Diagnostic::error(format!(
                                "ambiguous reference `{kind}.{s1}`, candidates: {}",
                                list.join(", ")
                            ))
                            .at(root_span.to(first.span))
                            .with_help(format!("qualify the reference, e.g. `{}`", list[0])),
                        ));
                    }
                    _ => None,
                },
            }
        } else {
            let address = Address::new(kind, vec![s1.clone()]);
            self.by_address.get(&address).map(|&id| (address, id, 1))
        };
        let Some((address, id, used)) = found else {
            // `table.app.users.x` reads as a qualified name when `app` is a known namespace.
            let qualified = spec.labels == LabelSpec::Qualified
                && s2.is_some()
                && candidates().any(|(a, _)| a.labels[0] == s1);
            let (written, end) = match (&s2, qualified) {
                (Some(s2), true) => (format!("{s1}.{s2}"), segments[1].span),
                _ => (s1.clone(), first.span),
            };
            let mut d =
                Diagnostic::error(format!("unknown {kind} `{written}`")).at(root_span.to(end));
            if self.has_for_each_block(None, kind, &written) {
                d = d.with_help(format!(
                    "`{kind} \"{written}\"` uses `for_each`; its instances cannot be referenced"
                ));
            } else if let Some(s) = suggest_address(&written, candidates().map(|(a, _)| a)) {
                d = d.with_help(format!("did you mean `{s}`?"));
            }
            return Err(self.error(d));
        };
        self.follow(address, id, root_span, &segments[used..], None)
    }

    /// Resolves the segments after a block reference: nested blocks and a final `.name`.
    /// `relative` is a nested kind whose label is the first segment (`column.id`).
    fn follow(
        &self,
        mut address: Address,
        mut id: NodeId,
        root_span: Span,
        mut rest: &[Segment],
        relative: Option<&str>,
    ) -> Eval {
        if let Some(kind) = relative {
            (address, id) = self.child(&address, id, kind, root_span, rest)?;
            rest = &rest[1..];
        }
        while let Some((seg, tail)) = rest.split_first() {
            let spec = self.nodes[id].spec;
            match &seg.key {
                Key::Name(name) if name == "name" && tail.is_empty() => {
                    return self.db_name(id).map(Value::String);
                }
                Key::Name(kind)
                    if spec
                        .nested
                        .iter()
                        .any(|b| b.kind == kind && b.referenceable) =>
                {
                    (address, id) = self.child(&address, id, kind, root_span, tail)?;
                    rest = &tail[1..];
                }
                key => {
                    let nested: Vec<String> = spec
                        .nested
                        .iter()
                        .filter(|b| b.referenceable)
                        .map(|b| format!("`.{}.<label>`", b.kind))
                        .collect();
                    let help = if nested.is_empty() {
                        "a reference can only be followed by `.name`".to_owned()
                    } else {
                        format!(
                            "a reference can be followed by `.name` or a nested block: {}",
                            nested.join(", ")
                        )
                    };
                    return Err(self.error(
                        Diagnostic::error(format!("unexpected `{key}` after `{address}`"))
                            .at(seg.span)
                            .with_help(help),
                    ));
                }
            }
        }
        Ok(Value::Ref(address))
    }

    /// The nested block of `kind` labelled by the first of `rest`.
    fn child(
        &self,
        parent: &Address,
        parent_id: NodeId,
        kind: &str,
        root_span: Span,
        rest: &[Segment],
    ) -> Eval<(Address, NodeId)> {
        let Some((seg, label)) = rest
            .first()
            .and_then(|seg| Some((seg, seg.key.as_label()?)))
        else {
            let at = rest.first().map_or(root_span, |s| s.span);
            return Err(
                self.error(Diagnostic::error(format!("expected a label after `.{kind}`")).at(at))
            );
        };
        let address = parent.child(kind, &label);
        if let Some(&id) = self.by_address.get(&address) {
            return Ok((address, id));
        }
        let mut d = Diagnostic::error(format!("unknown {kind} `{label}` in `{parent}`"))
            .at(root_span.to(seg.span));
        if self.has_for_each_block(Some(parent_id), kind, &label) {
            d = d.with_help(format!(
                "`{kind} \"{label}\"` uses `for_each`; its instances cannot be referenced"
            ));
        } else {
            let siblings = self.nodes[parent_id]
                .children
                .iter()
                .map(|&c| &self.nodes[c])
                .filter(|c| c.spec.kind == kind && c.address.is_some())
                .filter_map(|c| c.labels.first().map(String::as_str));
            if let Some(s) = suggest(&label, siblings) {
                d = d.with_help(format!("did you mean `{kind}.{s}`?"));
            }
        }
        Err(self.error(d))
    }

    fn has_for_each_block(&self, parent: Option<NodeId>, kind: &str, label: &str) -> bool {
        let ids: &[NodeId] = match parent {
            Some(id) => &self.nodes[id].children,
            None => &self.roots,
        };
        ids.iter().map(|&id| &self.nodes[id]).any(|n| {
            n.for_each.is_some()
                && n.spec.kind == kind
                && n.labels.first().is_some_and(|l| l == label)
        })
    }

    /// The database name of an addressed block: its `name` attribute when that is a string,
    /// otherwise its last label.
    fn db_name(&self, id: NodeId) -> Eval<String> {
        Ok(match self.name_value(id)? {
            Value::String(name) => name,
            _ => self.nodes[id]
                .address
                .as_ref()
                .map(|a| a.leaf_label().to_owned())
                .unwrap_or_default(),
        })
    }

    /// Applies `.attr` / `[index]` segments to a value. References continue to resolve nested
    /// blocks and `.name`.
    pub(super) fn index_value(&self, mut value: Value, segments: &[Segment]) -> Eval {
        for (i, seg) in segments.iter().enumerate() {
            value = match value {
                Value::Ref(address) => {
                    let Some(&id) = self.by_address.get(&address) else {
                        return Err(self.error(
                            Diagnostic::error(format!("unknown block `{address}`")).at(seg.span),
                        ));
                    };
                    return self.follow(address, id, seg.span, &segments[i..], None);
                }
                Value::List(mut items) => {
                    let index = match &seg.key {
                        Key::Value(Value::Number(n)) => n.as_i64(),
                        _ => None,
                    };
                    match index.and_then(|i| usize::try_from(i).ok()) {
                        Some(i) if i < items.len() => items.swap_remove(i),
                        Some(_) => {
                            return Err(self.error(
                                Diagnostic::error(format!(
                                    "index {} is out of range for a list of {} elements",
                                    seg.key.as_label().unwrap_or_default(),
                                    items.len()
                                ))
                                .at(seg.span),
                            ));
                        }
                        None => {
                            return Err(self.error(
                                Diagnostic::error(format!(
                                    "a list is indexed by a whole number, not `{}`",
                                    seg.key
                                ))
                                .at(seg.span),
                            ));
                        }
                    }
                }
                Value::Object(mut map) => {
                    let Some(key) = seg.key.as_label() else {
                        return Err(self.error(
                            Diagnostic::error(format!(
                                "an object is indexed by a string, not `{}`",
                                seg.key
                            ))
                            .at(seg.span),
                        ));
                    };
                    match map.swap_remove(&key) {
                        Some(v) => v,
                        None => {
                            let mut d =
                                Diagnostic::error(format!("the object has no attribute `{key}`"))
                                    .at(seg.span);
                            if let Some(s) = suggest(&key, map.keys().map(String::as_str)) {
                                d = d.with_help(format!("did you mean `{s}`?"));
                            }
                            return Err(self.error(d));
                        }
                    }
                }
                other => {
                    return Err(self.error(
                        Diagnostic::error(format!(
                            "cannot access `{}` on {}",
                            seg.key,
                            article(other.type_name())
                        ))
                        .at(seg.span),
                    ));
                }
            };
        }
        Ok(value)
    }
}

fn article(type_name: &str) -> String {
    match type_name {
        "null" => "null".to_owned(),
        t if t.starts_with(['a', 'e', 'i', 'o', 'u']) => format!("an {t}"),
        t => format!("a {t}"),
    }
}

/// The closest address to what was written, comparing both qualified (`app.users`) and bare
/// (`users`) forms.
fn suggest_address<'a>(
    written: &str,
    candidates: impl Iterator<Item = &'a Address>,
) -> Option<String> {
    candidates
        .map(|a| {
            let full = a.labels.join(".");
            let short = a.leaf_label();
            let score =
                strsim::jaro_winkler(written, &full).max(strsim::jaro_winkler(written, short));
            (score, a)
        })
        .filter(|(score, _)| *score > 0.8)
        .max_by(|x, y| x.0.total_cmp(&y.0))
        .map(|(_, a)| a.to_string())
}

trait SpanExt {
    fn to(self, end: Span) -> Span;
}

impl SpanExt for Span {
    /// From the start of `self` to the end of `end` (same file).
    fn to(self, end: Span) -> Span {
        Span::new(self.file, self.start, end.end.max(self.start))
    }
}
