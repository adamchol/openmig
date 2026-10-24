//! File discovery, parsing diagnostics and the structural pass: block kinds, labels, addresses,
//! duplicates and the `variable` / `locals` declarations.

use super::span;
use super::vars::{self, VarType};
use crate::decode::suggest;
use crate::diag::Diagnostic;
use crate::source::{FileId, SourceMap, Span};
use crate::spec::{BlockSpec, LabelSpec, SchemaSpec};
use crate::value::Address;
use hcl_edit::Span as _;
use hcl_edit::structure::{Attribute, Block as AstBlock, Body, Structure};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) type NodeId = usize;

/// A block of a known kind, before evaluation.
pub(super) struct Node<'a> {
    pub spec: &'static BlockSpec,
    pub ast: &'a AstBlock,
    pub file: FileId,
    pub parent: Option<NodeId>,
    /// Labels as written.
    pub labels: Vec<String>,
    /// `None` for unlabelled blocks, blocks with invalid labels, and blocks created by `for_each`
    /// (or nested in one).
    pub address: Option<Address>,
    pub for_each: Option<&'a Attribute>,
    pub children: Vec<NodeId>,
    pub span: Span,
    pub header_span: Span,
}

pub(super) struct VarDecl<'a> {
    pub name: String,
    pub file: FileId,
    /// `None` when the type expression is invalid (already reported).
    pub ty: Option<VarType>,
    pub default: Option<&'a Attribute>,
    pub header_span: Span,
}

pub(super) struct LocalDecl<'a> {
    pub name: String,
    pub file: FileId,
    pub attr: &'a Attribute,
}

pub(super) struct Collected<'a> {
    pub nodes: Vec<Node<'a>>,
    pub roots: Vec<NodeId>,
    pub variables: Vec<VarDecl<'a>>,
    pub locals: Vec<LocalDecl<'a>>,
    pub by_address: BTreeMap<Address, NodeId>,
    pub by_name: BTreeMap<(&'static str, String), Vec<NodeId>>,
}

/// Expands directories into the `*.hcl` files below them. The result is sorted and deduplicated.
pub(super) fn find_files(paths: &[PathBuf], diags: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => walk(path, &mut files, diags),
            Ok(_) => files.push(path.clone()),
            Err(e) => diags.push(Diagnostic::error(format!(
                "cannot read `{}`: {e}",
                path.display()
            ))),
        }
    }
    files.sort();
    files.dedup();
    files
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, diags: &mut Vec<Diagnostic>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            diags.push(Diagnostic::error(format!(
                "cannot read directory `{}`: {e}",
                dir.display()
            )));
            return;
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                diags.push(Diagnostic::error(format!(
                    "cannot read directory `{}`: {e}",
                    dir.display()
                )));
                continue;
            }
        };
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => walk(&path, files, diags),
            Ok(_) if path.extension().is_some_and(|e| e == "hcl") => files.push(path),
            Ok(_) => {}
            Err(e) => diags.push(Diagnostic::error(format!(
                "cannot read `{}`: {e}",
                path.display()
            ))),
        }
    }
}

pub(super) fn parse_error(
    sources: &SourceMap,
    file: FileId,
    error: &hcl_edit::parser::Error,
) -> Diagnostic {
    let content = &sources.get(file).content;
    let mut start = error.location().offset().min(content.len());
    while !content.is_char_boundary(start) {
        start -= 1;
    }
    let end = start + content[start..].chars().next().map_or(0, char::len_utf8);
    Diagnostic::error(format!("syntax error: {}", error.message())).at(Span::new(file, start, end))
}

pub(super) fn collect<'a>(
    spec: &SchemaSpec,
    bodies: &'a [(FileId, Body)],
    diags: &mut Vec<Diagnostic>,
) -> Collected<'a> {
    let mut c = Collector {
        spec,
        diags,
        out: Collected {
            nodes: Vec::new(),
            roots: Vec::new(),
            variables: Vec::new(),
            locals: Vec::new(),
            by_address: BTreeMap::new(),
            by_name: BTreeMap::new(),
        },
    };
    let kinds: &'static [BlockSpec] = spec.blocks;
    for (file, body) in bodies {
        let file = *file;
        for structure in body.iter() {
            let block = match structure {
                Structure::Block(block) => block,
                Structure::Attribute(attr) => {
                    c.diags.push(
                        Diagnostic::error(format!(
                            "unexpected attribute `{}` outside of a block",
                            attr.key.as_str()
                        ))
                        .at(span(file, attr.span()))
                        .with_help("define shared values in a `locals` block"),
                    );
                    continue;
                }
            };
            match block.ident.as_str() {
                "variable" => c.variable(file, block),
                "locals" => c.locals(file, block),
                kind => match kinds.iter().find(|b| b.kind == kind) {
                    Some(block_spec) => {
                        let id = c.node(block_spec, block, file, None, false);
                        c.out.roots.push(id);
                    }
                    None => {
                        let mut valid: Vec<&str> = kinds.iter().map(|b| b.kind).collect();
                        valid.extend(["variable", "locals"]);
                        c.diags.push(
                            Diagnostic::error(format!("unknown block kind `{kind}`"))
                                .at(span(file, block.ident.span()))
                                .with_help(kind_help(kind, &valid)),
                        );
                    }
                },
            }
        }
    }
    c.out
}

fn kind_help(kind: &str, valid: &[&str]) -> String {
    match suggest(kind, valid.iter().copied()) {
        Some(s) => format!("did you mean `{s}`?"),
        None if valid.is_empty() => "no nested blocks are allowed here".to_owned(),
        None => format!("expected one of: {}", valid.join(", ")),
    }
}

fn header_span(file: FileId, block: &AstBlock) -> Span {
    let ident = span(file, block.ident.span());
    let end = block
        .labels
        .last()
        .and_then(|l| l.span())
        .map_or(ident.end, |r| r.end);
    Span::new(file, ident.start, end)
}

struct Collector<'a, 'd> {
    spec: &'d SchemaSpec,
    diags: &'d mut Vec<Diagnostic>,
    out: Collected<'a>,
}

impl<'a> Collector<'a, '_> {
    fn node(
        &mut self,
        spec: &'static BlockSpec,
        ast: &'a AstBlock,
        file: FileId,
        parent: Option<NodeId>,
        in_instance: bool,
    ) -> NodeId {
        let labels: Vec<String> = ast.labels.iter().map(|l| l.as_str().to_owned()).collect();
        let header_span = header_span(file, ast);
        let canonical = self.check_labels(spec, ast, file, &labels, header_span);
        let for_each = ast.body.get_attribute("for_each");
        let instance = in_instance || for_each.is_some();
        let address = canonical
            .filter(|_| !instance)
            .and_then(|canonical| match parent {
                None => Some(Address::new(spec.kind, canonical)),
                Some(parent) => self.out.nodes[parent]
                    .address
                    .as_ref()
                    .map(|a| a.child(spec.kind, labels[0].clone())),
            });
        let id = self.out.nodes.len();
        self.out.nodes.push(Node {
            spec,
            ast,
            file,
            parent,
            labels,
            address: address.clone(),
            for_each,
            children: Vec::new(),
            span: span(file, ast.span()),
            header_span,
        });
        if let Some(address) = address {
            self.register(id, address);
        }
        let mut children = Vec::new();
        for child in ast.body.blocks() {
            let kind = child.ident.as_str();
            match spec.nested.iter().find(|b| b.kind == kind) {
                Some(child_spec) => {
                    children.push(self.node(child_spec, child, file, Some(id), instance));
                }
                None => {
                    let valid: Vec<&str> = spec.nested.iter().map(|b| b.kind).collect();
                    self.diags.push(
                        Diagnostic::error(format!(
                            "unknown block kind `{kind}` inside `{}`",
                            spec.kind
                        ))
                        .at(span(file, child.ident.span()))
                        .with_help(kind_help(kind, &valid)),
                    );
                }
            }
        }
        self.out.nodes[id].children = children;
        id
    }

    /// Validates labels and returns the canonical labels of a labelled block.
    fn check_labels(
        &mut self,
        spec: &BlockSpec,
        ast: &AstBlock,
        file: FileId,
        labels: &[String],
        header_span: Span,
    ) -> Option<Vec<String>> {
        let kind = spec.kind;
        let labels_span = |from: usize| {
            let start = ast.labels[from].span().map_or(0, |r| r.start);
            let end = ast
                .labels
                .last()
                .and_then(|l| l.span())
                .map_or(0, |r| r.end);
            Span::new(file, start, end)
        };
        if spec.labels == LabelSpec::None {
            if !labels.is_empty() {
                self.diags.push(
                    Diagnostic::error(format!("`{kind}` blocks take no labels")).at(labels_span(0)),
                );
            }
            return None;
        }
        if labels.len() > 1 {
            self.diags.push(
                Diagnostic::error(format!(
                    "`{kind}` blocks take one label, found {}",
                    labels.len()
                ))
                .at(labels_span(1))
                .with_label("unexpected label"),
            );
            return None;
        }
        let Some(label) = labels.first() else {
            if spec.labels != LabelSpec::OptionalOne {
                let example = if spec.labels == LabelSpec::Qualified {
                    "namespace.name"
                } else {
                    "name"
                };
                self.diags.push(
                    Diagnostic::error(format!("`{kind}` blocks need a label"))
                        .at(header_span)
                        .with_help(format!("write `{kind} \"{example}\" {{ ... }}`")),
                );
            }
            return None;
        };
        if label.is_empty() {
            self.diags
                .push(Diagnostic::error("labels cannot be empty").at(labels_span(0)));
            return None;
        }
        if spec.labels != LabelSpec::Qualified {
            return Some(vec![label.clone()]);
        }
        match label.split_once('.') {
            None => Some(vec![self.spec.default_namespace.clone(), label.clone()]),
            Some((namespace, name)) if !namespace.is_empty() && !name.is_empty() => {
                Some(vec![namespace.to_owned(), name.to_owned()])
            }
            Some(_) => {
                self.diags.push(
                    Diagnostic::error(format!("invalid label `{label}`"))
                        .at(labels_span(0))
                        .with_help("write `namespace.name`, or `name` for the default namespace"),
                );
                None
            }
        }
    }

    fn register(&mut self, id: NodeId, address: Address) {
        let node = &self.out.nodes[id];
        if let Some(&first) = self.out.by_address.get(&address) {
            if !node.spec.mergeable {
                self.diags.push(
                    Diagnostic::error(format!("`{address}` is declared more than once"))
                        .at(node.header_span)
                        .with_label("declared again here")
                        .with_related(self.out.nodes[first].header_span, "first declared here"),
                );
            }
            return;
        }
        if node.parent.is_none() && node.spec.labels == LabelSpec::Qualified {
            self.out
                .by_name
                .entry((node.spec.kind, address.labels[1].clone()))
                .or_default()
                .push(id);
        }
        self.out.by_address.insert(address, id);
    }

    fn variable(&mut self, file: FileId, block: &'a AstBlock) {
        let header_span = header_span(file, block);
        let name = match block.labels.as_slice() {
            [label] if !label.as_str().is_empty() => label.as_str().to_owned(),
            [] => {
                self.diags.push(
                    Diagnostic::error("`variable` blocks need a name")
                        .at(header_span)
                        .with_help("write `variable \"name\" { ... }`"),
                );
                return;
            }
            _ => {
                self.diags.push(
                    Diagnostic::error("`variable` blocks take one non-empty label").at(header_span),
                );
                return;
            }
        };
        let mut ty = Some(VarType::Any);
        let mut default = None;
        for structure in block.body.iter() {
            match structure {
                Structure::Attribute(attr) => match attr.key.as_str() {
                    "type" => {
                        ty = vars::parse_type(&attr.value)
                            .map_err(|message| {
                                self.diags.push(
                                    Diagnostic::error(message).at(span(file, attr.value.span())),
                                );
                            })
                            .ok();
                    }
                    "default" => default = Some(attr),
                    "description" => {}
                    other => {
                        let mut d =
                            Diagnostic::error(format!("unknown attribute `{other}` in `variable`"))
                                .at(span(file, attr.span()));
                        if let Some(s) = suggest(other, ["type", "default", "description"]) {
                            d = d.with_help(format!("did you mean `{s}`?"));
                        }
                        self.diags.push(d);
                    }
                },
                Structure::Block(nested) => self.diags.push(
                    Diagnostic::error(format!(
                        "unexpected block `{}` inside `variable`",
                        nested.ident.as_str()
                    ))
                    .at(span(file, nested.ident.span())),
                ),
            }
        }
        if let Some(first) = self.out.variables.iter().find(|v| v.name == name) {
            self.diags.push(
                Diagnostic::error(format!("variable `{name}` is declared more than once"))
                    .at(header_span)
                    .with_label("declared again here")
                    .with_related(first.header_span, "first declared here"),
            );
            return;
        }
        self.out.variables.push(VarDecl {
            name,
            file,
            ty,
            default,
            header_span,
        });
    }

    fn locals(&mut self, file: FileId, block: &'a AstBlock) {
        if !block.labels.is_empty() {
            self.diags.push(
                Diagnostic::error("`locals` blocks take no labels").at(header_span(file, block)),
            );
        }
        for structure in block.body.iter() {
            match structure {
                Structure::Attribute(attr) => {
                    let name = attr.key.as_str();
                    let attr_span = span(file, attr.key.span());
                    if let Some(first) = self.out.locals.iter().find(|l| l.name == name) {
                        self.diags.push(
                            Diagnostic::error(format!(
                                "local value `{name}` is declared more than once"
                            ))
                            .at(attr_span)
                            .with_label("declared again here")
                            .with_related(
                                span(first.file, first.attr.key.span()),
                                "first declared here",
                            ),
                        );
                        continue;
                    }
                    self.out.locals.push(LocalDecl {
                        name: name.to_owned(),
                        file,
                        attr,
                    });
                }
                Structure::Block(nested) => self.diags.push(
                    Diagnostic::error(format!(
                        "unexpected block `{}` inside `locals`",
                        nested.ident.as_str()
                    ))
                    .at(span(file, nested.ident.span())),
                ),
            }
        }
    }
}
