//! The loading pipeline.
//!
//! 1. [`collect`] parses every file, validates block kinds and labels against the [`SchemaSpec`],
//!    assigns addresses and gathers `variable`/`locals` declarations. Blocks live in an arena of
//!    [`Node`]s that keep a reference to their syntax tree.
//! 2. Variables, locals and the `name` attribute of addressed blocks are evaluated lazily and
//!    memoized, so references may point forward and across files, and cycles are detected.
//! 3. Every block is expanded (`for_each`) and its attributes are evaluated into [`Value`]s.
//!
//! Errors are collected instead of aborting: a failed expression drops only its attribute.

mod collect;
mod expr;
mod refs;
mod template;
mod vars;

use crate::diag::{Diagnostic, Diagnostics, Severity};
use crate::doc::{Attr, Block, Document};
use crate::load::{LoadError, LoadOptions, VarValue};
use crate::source::{FileId, SourceMap, Span};
use crate::spec::SchemaSpec;
use crate::value::{Address, Value};
use collect::{Collected, LocalDecl, Node, NodeId, VarDecl};
use expr::{Each, Eval, Fail, Scope};
use hcl_edit::Span as _;
use hcl_edit::structure::Body;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::PathBuf;

pub(crate) fn load_paths(paths: &[PathBuf], opts: &LoadOptions<'_>) -> Result<Document, LoadError> {
    let mut diags = Vec::new();
    let mut files = Vec::new();
    for path in collect::find_files(paths, &mut diags) {
        match std::fs::read_to_string(&path) {
            Ok(content) => files.push((path, content)),
            Err(e) => diags.push(Diagnostic::error(format!(
                "cannot read `{}`: {e}",
                path.display()
            ))),
        }
    }
    load(files, opts, diags)
}

pub(crate) fn load_sources(
    sources: Vec<(PathBuf, String)>,
    opts: &LoadOptions<'_>,
) -> Result<Document, LoadError> {
    load(sources, opts, Vec::new())
}

fn load(
    files: Vec<(PathBuf, String)>,
    opts: &LoadOptions<'_>,
    mut diags: Vec<Diagnostic>,
) -> Result<Document, LoadError> {
    let mut sources = SourceMap::default();
    let mut bodies = Vec::new();
    for (path, content) in files {
        let parsed = content.parse::<Body>();
        let file = sources.add(path, content);
        match parsed {
            Ok(body) => bodies.push((file, body)),
            Err(e) => diags.push(collect::parse_error(&sources, file, &e)),
        }
    }
    let collected = collect::collect(opts.spec, &bodies, &mut diags);
    let loader = Loader::new(opts, &sources, collected, diags);
    let (blocks, diags) = loader.run();
    finish(sources, blocks, diags)
}

fn finish(
    sources: SourceMap,
    blocks: Vec<Block>,
    mut diags: Vec<Diagnostic>,
) -> Result<Document, LoadError> {
    // `for_each` instances evaluate the same expressions once per instance; report each problem
    // once.
    diags.sort_by(|a, b| {
        let key = |d: &Diagnostic| d.span.map(|s| (s.file, s.start, s.end));
        key(a).cmp(&key(b)).then_with(|| a.message.cmp(&b.message))
    });
    diags.dedup_by(|a, b| a.span == b.span && a.message == b.message);
    let diagnostics = Diagnostics { items: diags };
    if diagnostics.has_errors() {
        return Err(LoadError {
            sources,
            diagnostics,
        });
    }
    Ok(Document {
        sources,
        blocks,
        warnings: diagnostics,
    })
}

pub(super) fn span(file: FileId, range: Option<Range<usize>>) -> Span {
    range.map_or(Span::new(file, 0, 0), |r| Span::new(file, r.start, r.end))
}

/// A lazily evaluated value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Var(usize),
    Local(usize),
    /// The `name` attribute of an addressed block.
    Name(NodeId),
}

#[derive(Clone)]
enum Memo {
    Pending,
    Done(Eval),
}

pub(super) struct Loader<'a> {
    spec: &'a SchemaSpec,
    sources: &'a SourceMap,
    given_vars: &'a IndexMap<String, VarValue>,
    nodes: Vec<Node<'a>>,
    roots: Vec<NodeId>,
    variables: Vec<VarDecl<'a>>,
    locals: Vec<LocalDecl<'a>>,
    /// Every addressed block (first declaration wins).
    by_address: BTreeMap<Address, NodeId>,
    /// Top-level blocks of qualified kinds by `(kind, name)`, for unqualified references.
    by_name: BTreeMap<(&'static str, String), Vec<NodeId>>,
    memos: RefCell<HashMap<Slot, Memo>>,
    /// Slots being evaluated, outermost first; used to describe cycles.
    stack: RefCell<Vec<Slot>>,
    /// Diagnostics of the evaluation in progress. `try()` truncates this buffer.
    diags: RefCell<Vec<Diagnostic>>,
    /// Diagnostics that no `try()` may discard: those of memoized evaluations, which are reported
    /// once no matter where they were first requested.
    settled: RefCell<Vec<Diagnostic>>,
}

impl<'a> Loader<'a> {
    fn new(
        opts: &'a LoadOptions<'a>,
        sources: &'a SourceMap,
        collected: Collected<'a>,
        diags: Vec<Diagnostic>,
    ) -> Self {
        Self {
            spec: opts.spec,
            sources,
            given_vars: &opts.vars,
            nodes: collected.nodes,
            roots: collected.roots,
            variables: collected.variables,
            locals: collected.locals,
            by_address: collected.by_address,
            by_name: collected.by_name,
            memos: RefCell::default(),
            stack: RefCell::default(),
            diags: RefCell::default(),
            settled: RefCell::new(diags),
        }
    }

    fn run(self) -> (Vec<Block>, Vec<Diagnostic>) {
        for (name, _) in self.given_vars {
            if !self.variables.iter().any(|v| &v.name == name) {
                let mut d = Diagnostic::warning(format!(
                    "a value was given for the undeclared variable `{name}`"
                ));
                if let Some(s) =
                    crate::decode::suggest(name, self.variables.iter().map(|v| v.name.as_str()))
                {
                    d = d.with_help(format!("did you mean `{s}`?"));
                }
                self.settled.borrow_mut().push(d);
            }
        }
        for (i, var) in self.variables.iter().enumerate() {
            let _ = self.var_value(i, var.header_span);
        }
        for (i, local) in self.locals.iter().enumerate() {
            let _ = self.local_value(i, span(local.file, local.attr.key.span()));
        }
        let mut blocks = Vec::new();
        for &root in &self.roots {
            self.expand(root, None, &mut blocks);
        }
        let mut diags = self.settled.into_inner();
        diags.extend(self.diags.into_inner());
        (blocks, diags)
    }

    /// Evaluates a node into one block, or one block per `for_each` instance.
    fn expand(&self, id: NodeId, each: Option<&Each>, out: &mut Vec<Block>) {
        let node = &self.nodes[id];
        let Some(for_each) = node.for_each else {
            out.push(self.block(id, each, None));
            return;
        };
        let scope = Scope {
            block: node.parent,
            each,
            ..Scope::new(node.file)
        };
        let Ok(collection) = self.eval(&for_each.value, &scope) else {
            return;
        };
        let instances: Vec<(String, Each)> = match collection {
            Value::List(items) => items
                .into_iter()
                .enumerate()
                .map(|(i, value)| {
                    let key = Value::Number((i as i64).into());
                    (i.to_string(), Each { key, value })
                })
                .collect(),
            Value::Object(map) => map
                .into_iter()
                .map(|(k, value)| {
                    let key = Value::String(k.clone());
                    (k, Each { key, value })
                })
                .collect(),
            other => {
                self.error(
                    Diagnostic::error(format!(
                        "`for_each` must be a list or an object, found {}",
                        other.type_name()
                    ))
                    .at(span(node.file, for_each.value.span())),
                );
                return;
            }
        };
        for (key, each) in instances {
            out.push(self.block(id, Some(&each), Some(key)));
        }
    }

    fn block(&self, id: NodeId, each: Option<&Each>, each_key: Option<String>) -> Block {
        let node = &self.nodes[id];
        let scope = Scope {
            block: Some(id),
            each,
            ..Scope::new(node.file)
        };
        let mut attrs = IndexMap::new();
        for attr in node.ast.body.attributes() {
            let name = attr.key.as_str();
            if name == "for_each" && node.for_each.is_some() {
                continue;
            }
            let value = if name == "name" && node.address.is_some() {
                self.name_value(id)
            } else {
                self.eval(&attr.value, &scope)
            };
            if let Ok(value) = value {
                attrs.insert(
                    name.to_owned(),
                    Attr {
                        name: name.to_owned(),
                        value,
                        span: span(node.file, attr.span()),
                        value_span: span(node.file, attr.value.span()),
                    },
                );
            }
        }
        let mut blocks = Vec::new();
        for &child in &node.children {
            self.expand(child, each, &mut blocks);
        }
        Block {
            kind: node.spec.kind.to_owned(),
            labels: node.labels.clone(),
            address: node.address.clone(),
            attrs,
            blocks,
            span: node.span,
            header_span: node.header_span,
            each_key,
        }
    }

    fn error(&self, diagnostic: Diagnostic) -> Fail {
        debug_assert_eq!(diagnostic.severity, Severity::Error);
        self.diags.borrow_mut().push(diagnostic);
        Fail
    }

    /// Runs `f` with its own diagnostics buffer and moves what it reports to `settled`.
    fn isolated<T>(&self, f: impl FnOnce() -> T) -> T {
        let outer = self.diags.take();
        let result = f();
        let produced = self.diags.replace(outer);
        self.settled.borrow_mut().extend(produced);
        result
    }

    /// Evaluates `slot` once. `at` is where the value is requested, used to report cycles.
    fn memoized(&self, slot: Slot, at: Span, compute: impl FnOnce() -> Eval) -> Eval {
        let state = self.memos.borrow().get(&slot).cloned();
        match state {
            Some(Memo::Done(result)) => return result,
            Some(Memo::Pending) => return Err(self.cycle(slot, at)),
            None => {}
        }
        self.memos.borrow_mut().insert(slot, Memo::Pending);
        self.stack.borrow_mut().push(slot);
        let result = self.isolated(compute);
        self.stack.borrow_mut().pop();
        self.memos
            .borrow_mut()
            .insert(slot, Memo::Done(result.clone()));
        result
    }

    fn cycle(&self, slot: Slot, at: Span) -> Fail {
        let chain = {
            let stack = self.stack.borrow();
            let start = stack.iter().position(|s| *s == slot).unwrap_or(0);
            stack[start..]
                .iter()
                .chain([&slot])
                .map(|s| self.describe(*s))
                .collect::<Vec<_>>()
                .join(" -> ")
        };
        self.error(
            Diagnostic::error(format!("`{}` depends on itself", self.describe(slot)))
                .at(at)
                .with_label("cyclic reference")
                .with_help(format!("cycle: {chain}")),
        )
    }

    fn describe(&self, slot: Slot) -> String {
        match slot {
            Slot::Var(i) => format!("var.{}", self.variables[i].name),
            Slot::Local(i) => format!("local.{}", self.locals[i].name),
            Slot::Name(id) => match &self.nodes[id].address {
                Some(address) => format!("{address}.name"),
                None => "name".to_owned(),
            },
        }
    }

    fn var_value(&self, index: usize, at: Span) -> Eval {
        self.memoized(Slot::Var(index), at, || self.compute_var(index))
    }

    fn compute_var(&self, index: usize) -> Eval {
        let decl = &self.variables[index];
        let scope = Scope {
            var_default: true,
            ..Scope::new(decl.file)
        };
        let default = decl
            .default
            .map(|attr| (self.eval(&attr.value, &scope), attr));
        let ty = decl.ty.as_ref().ok_or(Fail)?;
        let invalid = |what: &str, message: String, at: Span| {
            self.error(
                Diagnostic::error(format!(
                    "invalid {what} for variable `{}`: {message}",
                    decl.name
                ))
                .at(at),
            )
        };
        match self.given_vars.get(&decl.name) {
            Some(VarValue::Raw(text)) => vars::from_raw(text, ty)
                .map_err(|message| invalid("value", message, decl.header_span)),
            Some(VarValue::Value(value)) => vars::convert(value.clone(), ty)
                .map_err(|message| invalid("value", message, decl.header_span)),
            None => match default {
                Some((Ok(value), attr)) => vars::convert(value, ty).map_err(|message| {
                    invalid("default", message, span(decl.file, attr.value.span()))
                }),
                Some((Err(fail), _)) => Err(fail),
                None => Err(self.error(
                    Diagnostic::error(format!("variable `{}` has no value", decl.name))
                        .at(decl.header_span)
                        .with_help(format!(
                            "give it a `default`, or set it with `--var {}=...` or in \
                             `[databases.<name>.vars]`",
                            decl.name
                        )),
                )),
            },
        }
    }

    fn local_value(&self, index: usize, at: Span) -> Eval {
        self.memoized(Slot::Local(index), at, || {
            let local = &self.locals[index];
            self.eval(&local.attr.value, &Scope::new(local.file))
        })
    }

    /// The evaluated `name` attribute of an addressed block (`Null` when absent).
    fn name_value(&self, id: NodeId) -> Eval {
        let node = &self.nodes[id];
        let Some(attr) = node.ast.body.get_attribute("name") else {
            return Ok(Value::Null);
        };
        let at = span(node.file, attr.value.span());
        self.memoized(Slot::Name(id), at, || {
            let scope = Scope {
                block: Some(id),
                ..Scope::new(node.file)
            };
            self.eval(&attr.value, &scope)
        })
    }
}
