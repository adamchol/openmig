//! A PostgreSQL-like vocabulary and helpers shared by the integration tests.
#![allow(dead_code)]

use indexmap::IndexMap;
use openmig_hcl::load::LoadError;
use openmig_hcl::{
    Address, Block, BlockSpec, Document, LabelSpec, LoadOptions, Number, SchemaSpec, Value,
    VarValue, load_sources,
};
use std::path::PathBuf;

static KEY: &[BlockSpec] = &[BlockSpec::new("key", LabelSpec::None)];

static TABLE: &[BlockSpec] = &[
    BlockSpec::new("column", LabelSpec::One).referenceable(),
    BlockSpec::new("index", LabelSpec::OptionalOne)
        .referenceable()
        .nested(KEY),
    BlockSpec::new("primary_key", LabelSpec::OptionalOne),
    BlockSpec::new("foreign_key", LabelSpec::One),
    BlockSpec::new("policy", LabelSpec::One),
    BlockSpec::new("trigger", LabelSpec::One),
];

static FUNCTION: &[BlockSpec] = &[BlockSpec::new("arg", LabelSpec::OptionalOne)];

static BLOCKS: &[BlockSpec] = &[
    BlockSpec::new("schema", LabelSpec::One).referenceable(),
    BlockSpec::new("table", LabelSpec::Qualified)
        .referenceable()
        .nested(TABLE),
    BlockSpec::new("function", LabelSpec::Qualified)
        .referenceable()
        .nested(FUNCTION),
    BlockSpec::new("role", LabelSpec::One).referenceable(),
    BlockSpec::new("grant", LabelSpec::None),
    BlockSpec::new("data", LabelSpec::Qualified)
        .referenceable()
        .mergeable(),
];

pub fn spec() -> SchemaSpec {
    SchemaSpec {
        blocks: BLOCKS,
        default_namespace: "public".to_owned(),
    }
}

pub fn load_files_with(
    files: &[(&str, &str)],
    vars: &[(&str, VarValue)],
) -> Result<Document, LoadError> {
    let spec = spec();
    let opts = LoadOptions {
        spec: &spec,
        vars: vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<IndexMap<_, _>>(),
    };
    let sources = files
        .iter()
        .map(|(path, content)| (PathBuf::from(path), (*content).to_owned()))
        .collect();
    load_sources(sources, &opts)
}

pub fn load_files(files: &[(&str, &str)]) -> Result<Document, LoadError> {
    load_files_with(files, &[])
}

/// Loads one file, panicking with the rendered diagnostics on failure.
pub fn load_ok(src: &str) -> Document {
    load_files(&[("main.hcl", src)]).unwrap_or_else(|e| panic!("load failed:\n{}", e.render(false)))
}

/// Loads one file and returns the rendered diagnostics, panicking if loading succeeds.
pub fn load_err(src: &str) -> String {
    errors(load_files(&[("main.hcl", src)]))
}

pub fn errors(result: Result<Document, LoadError>) -> String {
    match result {
        Ok(_) => panic!("expected the load to fail"),
        Err(e) => e.render(false),
    }
}

/// The messages of all diagnostics.
pub fn messages(result: Result<Document, LoadError>) -> Vec<String> {
    match result {
        Ok(_) => panic!("expected the load to fail"),
        Err(e) => e
            .diagnostics
            .items
            .iter()
            .map(|d| d.message.clone())
            .collect(),
    }
}

pub fn err_messages(src: &str) -> Vec<String> {
    messages(load_files(&[("main.hcl", src)]))
}

/// The value of attribute `name` of the first block of `kind`.
pub fn attr(doc: &Document, kind: &str, name: &str) -> Value {
    let block = doc
        .blocks_of(kind)
        .next()
        .unwrap_or_else(|| panic!("no `{kind}` block"));
    block_attr(block, name)
}

pub fn block_attr(block: &Block, name: &str) -> Value {
    block
        .attr(name)
        .unwrap_or_else(|| panic!("no attribute `{name}` in `{}`", block.kind))
        .value
        .clone()
}

/// Evaluates `expr` as the attribute `v` of a `grant` block.
pub fn eval(expr: &str) -> Value {
    let doc = load_ok(&format!("grant {{\n  v = {expr}\n}}\n"));
    attr(&doc, "grant", "v")
}

/// Evaluates `expr` and returns the error messages.
pub fn eval_err(expr: &str) -> Vec<String> {
    err_messages(&format!("grant {{\n  v = {expr}\n}}\n"))
}

pub fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

pub fn n(i: i64) -> Value {
    Value::Number(Number::Int(i))
}

pub fn ident(v: &str) -> Value {
    Value::Ident(v.to_owned())
}

pub fn list(items: Vec<Value>) -> Value {
    Value::List(items)
}

pub fn obj(entries: &[(&str, Value)]) -> Value {
    Value::Object(
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

pub fn call(name: &str, args: Vec<Value>) -> Value {
    Value::Call {
        name: name.to_owned(),
        args,
    }
}

/// `Address` from a dotted path: `addr("table", &["app", "users"], &[("column", "id")])`.
pub fn addr(kind: &str, labels: &[&str], nested: &[(&str, &str)]) -> Address {
    Address {
        kind: kind.to_owned(),
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        nested: nested
            .iter()
            .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
            .collect(),
    }
}

pub fn reference(kind: &str, labels: &[&str], nested: &[(&str, &str)]) -> Value {
    Value::Ref(addr(kind, labels, nested))
}
