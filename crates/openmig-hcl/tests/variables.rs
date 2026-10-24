//! Variables and locals.

mod common;

use common::*;
use openmig_hcl::{Value, VarValue};
use pretty_assertions::assert_eq;

const VARS: &str = r#"
variable "replicated" {
  type    = bool
  default = true
}
variable "db" {
  type        = string
  description = "Database name"
}
variable "shards" {
  type    = number
  default = 1
}
variable "roles" {
  type    = list(string)
  default = ["web"]
}
variable "settings" {
  type    = map(any)
  default = {}
}
variable "anything" {
  default = null
}
grant {
  replicated = var.replicated
  db         = var.db
  shards     = var.shards
  roles      = var.roles
  settings   = var.settings
  anything   = var.anything
}
"#;

fn with_vars(vars: &[(&str, VarValue)]) -> openmig_hcl::Document {
    load_files_with(&[("main.hcl", VARS)], vars).unwrap_or_else(|e| panic!("{}", e.render(false)))
}

#[test]
fn defaults_and_given_values() {
    let doc = with_vars(&[("db", VarValue::Raw("dialo".into()))]);
    assert_eq!(attr(&doc, "grant", "replicated"), Value::Bool(true));
    assert_eq!(attr(&doc, "grant", "db"), s("dialo"));
    assert_eq!(attr(&doc, "grant", "shards"), n(1));
    assert_eq!(attr(&doc, "grant", "roles"), list(vec![s("web")]));
    assert_eq!(attr(&doc, "grant", "settings"), obj(&[]));
    assert_eq!(attr(&doc, "grant", "anything"), Value::Null);
}

#[test]
fn raw_values_are_converted_to_the_declared_type() {
    let doc = with_vars(&[
        ("db", VarValue::Raw("dialo".into())),
        ("replicated", VarValue::Raw("false".into())),
        ("shards", VarValue::Raw("3".into())),
        ("roles", VarValue::Raw(r#"["a", "b"]"#.into())),
        ("settings", VarValue::Raw(r#"{"x": 1}"#.into())),
        ("anything", VarValue::Raw("text".into())),
    ]);
    assert_eq!(attr(&doc, "grant", "replicated"), Value::Bool(false));
    assert_eq!(attr(&doc, "grant", "shards"), n(3));
    assert_eq!(attr(&doc, "grant", "roles"), list(vec![s("a"), s("b")]));
    assert_eq!(attr(&doc, "grant", "settings"), obj(&[("x", n(1))]));
    assert_eq!(attr(&doc, "grant", "anything"), s("text"));
}

#[test]
fn typed_values_are_checked() {
    let doc = with_vars(&[
        ("db", VarValue::Value(s("dialo"))),
        ("replicated", VarValue::Value(Value::Bool(false))),
        ("shards", VarValue::Value(s("2"))),
    ]);
    assert_eq!(attr(&doc, "grant", "replicated"), Value::Bool(false));
    assert_eq!(attr(&doc, "grant", "shards"), n(2));

    let messages = messages(load_files_with(
        &[("main.hcl", VARS)],
        &[
            ("db", VarValue::Value(Value::List(vec![]))),
            ("replicated", VarValue::Raw("maybe".into())),
        ],
    ));
    assert_eq!(
        messages,
        [
            "invalid value for variable `replicated`: expected `true` or `false`, found `maybe`",
            "invalid value for variable `db`: expected a string, found list",
        ]
    );
}

#[test]
fn missing_values_are_errors() {
    let rendered = errors(load_files(&[("main.hcl", VARS)]));
    assert!(
        rendered.contains("variable `db` has no value"),
        "{rendered}"
    );
    assert!(rendered.contains("--var db=..."), "{rendered}");
}

#[test]
fn undeclared_values_are_warnings() {
    let doc = with_vars(&[
        ("db", VarValue::Raw("dialo".into())),
        ("dbb", VarValue::Raw("x".into())),
    ]);
    let warnings: Vec<&str> = doc
        .warnings
        .items
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        warnings,
        ["a value was given for the undeclared variable `dbb`"]
    );
    assert_eq!(
        doc.warnings.items[0].help.as_deref(),
        Some("did you mean `db`?")
    );
    assert!(!doc.warnings.has_errors());
}

#[test]
fn invalid_defaults_and_types() {
    let messages = err_messages(
        r#"
variable "a" {
  type    = number
  default = "x"
}
variable "b" {
  type = strin
}
variable "c" {
  default = var.a
}
variable "d" {
  sensitive = true
  default   = 1
}
variable "a" {
  default = 1
}
"#,
    );
    assert_eq!(
        messages,
        [
            "invalid default for variable `a`: expected a number, found `x`",
            "unknown type `strin`; expected a type: string, number, bool, any, list(T), set(T), map(T) or object({...})",
            "a variable default cannot use `var`",
            "unknown attribute `sensitive` in `variable`",
            "variable `a` is declared more than once",
        ]
    );
}

#[test]
fn unknown_variables_suggest_declared_ones() {
    let rendered =
        load_err("variable \"region\" {\n  default = \"eu\"\n}\ngrant {\n  v = var.regoin\n}\n");
    assert!(rendered.contains("unknown variable `regoin`"), "{rendered}");
    assert!(rendered.contains("did you mean `region`?"), "{rendered}");
}

#[test]
fn variable_values_can_be_indexed() {
    let doc = load_ok(
        r#"
variable "cfg" {
  default = { hosts = ["a", "b"], port = 5432 }
}
grant {
  host = var.cfg.hosts[1]
  port = var.cfg["port"]
}
"#,
    );
    assert_eq!(attr(&doc, "grant", "host"), s("b"));
    assert_eq!(attr(&doc, "grant", "port"), n(5432));
    assert_eq!(
        err_messages("variable \"cfg\" {\n  default = { a = 1 }\n}\ngrant {\n  v = var.cfg.b\n}\n"),
        ["the object has no attribute `b`"]
    );
}

#[test]
fn locals_evaluate_lazily_in_any_order() {
    let doc = load_files(&[
        ("a.hcl", "grant {\n  v = local.full\n  r = local.roles\n}\n"),
        (
            "b.hcl",
            r#"
locals {
  full = "${local.prefix}_${var.env}"
}
locals {
  prefix = upper("app")
  roles  = [role.web]
}
variable "env" {
  default = "prod"
}
role "web" {}
"#,
        ),
    ])
    .unwrap();
    assert_eq!(attr(&doc, "grant", "v"), s("APP_prod"));
    assert_eq!(
        attr(&doc, "grant", "r"),
        list(vec![reference("role", &["web"], &[])])
    );
}

#[test]
fn local_cycles_are_errors() {
    let rendered = load_err("locals {\n  a = local.b\n  b = local.c\n  c = local.a\n}\n");
    assert!(
        rendered.contains("`local.a` depends on itself"),
        "{rendered}"
    );
    assert!(
        rendered.contains("cycle: local.a -> local.b -> local.c -> local.a"),
        "{rendered}"
    );
    assert_eq!(
        rendered.matches("depends on itself").count(),
        1,
        "{rendered}"
    );
}

#[test]
fn local_errors_are_reported_once() {
    let messages = err_messages(
        "locals {\n  a = 1 + \"x\"\n}\ngrant {\n  v = local.a\n  w = try(local.a, 0)\n}\n",
    );
    assert_eq!(messages, ["`+` needs numbers, found number and string"]);
}

#[test]
fn duplicate_and_unknown_locals() {
    let messages =
        err_messages("locals {\n  a = 1\n}\nlocals {\n  a = 2\n}\ngrant {\n  v = local.b\n}\n");
    assert_eq!(
        messages,
        [
            "local value `a` is declared more than once",
            "unknown local value `b`"
        ]
    );
    assert_eq!(
        err_messages("locals \"x\" {\n  a = 1\n}\n"),
        ["`locals` blocks take no labels"]
    );
    assert_eq!(eval_err("var"), ["`var` must be followed by a name"]);
}
