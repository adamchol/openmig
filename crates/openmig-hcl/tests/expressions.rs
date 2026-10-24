//! Literals, templates, heredocs, operators, conditionals, `for` expressions and SQL-shaped values.

mod common;

use common::*;
use openmig_hcl::{Number, Value};
use pretty_assertions::assert_eq;

#[test]
fn literals() {
    assert_eq!(eval("null"), Value::Null);
    assert_eq!(eval("true"), Value::Bool(true));
    assert_eq!(eval("42"), n(42));
    assert_eq!(eval("-7"), n(-7));
    assert_eq!(eval("1.5"), Value::Number(Number::Float(1.5)));
    assert_eq!(eval("1.50"), Value::Number(Number::Text("1.50".into())));
    assert_eq!(eval("1e10"), Value::Number(Number::Text("1e10".into())));
    assert_eq!(
        eval("18446744073709551615"),
        Value::Number(Number::Text("18446744073709551615".into()))
    );
    assert_eq!(eval("\"a\\tb\\\"c\\u00e9\""), s("a\tb\"cé"));
    assert_eq!(
        eval("[1, \"a\", [true]]"),
        list(vec![n(1), s("a"), list(vec![Value::Bool(true)])])
    );
    assert_eq!(
        eval("{ a = 1, \"b c\" = 2, (\"d\") = 3, e: 4 }"),
        obj(&[("a", n(1)), ("b c", n(2)), ("d", n(3)), ("e", n(4))])
    );
}

#[test]
fn object_keys_must_be_unique_strings() {
    assert_eq!(
        eval_err("{ \"a\" = 1, a = 2 }"),
        ["duplicate object key `a`"]
    );
    assert_eq!(
        eval_err("{ (null) = 1 }"),
        ["object keys must be strings, found null"]
    );
}

#[test]
fn identifiers_and_calls_are_kept() {
    assert_eq!(eval("text"), ident("text"));
    assert_eq!(
        eval("[SELECT, INSERT]"),
        list(vec![ident("SELECT"), ident("INSERT")])
    );
    assert_eq!(eval("varchar(255)"), call("varchar", vec![n(255)]));
    assert_eq!(eval("numeric(10, 2)"), call("numeric", vec![n(10), n(2)]));
    assert_eq!(
        eval("Map(LowCardinality(String), String)"),
        call(
            "Map",
            vec![
                call("LowCardinality", vec![ident("String")]),
                ident("String")
            ]
        )
    );
    assert_eq!(eval("DateTime64(9)"), call("DateTime64", vec![n(9)]));
    assert_eq!(
        eval("[Delta(8), ZSTD(1)]"),
        list(vec![call("Delta", vec![n(8)]), call("ZSTD", vec![n(1)])])
    );
    assert_eq!(eval("MergeTree()"), call("MergeTree", vec![]));
}

#[test]
fn engine_calls_with_interpolation() {
    let doc = load_ok(
        r#"
variable "cluster" {
  default = "main"
}
grant {
  engine = ReplicatedMergeTree("/clickhouse/${var.cluster}/tables/{uuid}/{shard}", "{replica}")
}
"#,
    );
    assert_eq!(
        attr(&doc, "grant", "engine"),
        call(
            "ReplicatedMergeTree",
            vec![s("/clickhouse/main/tables/{uuid}/{shard}"), s("{replica}")]
        )
    );
}

#[test]
fn sql_values() {
    assert_eq!(eval("sql(\"now()\")"), Value::Sql("now()".into()));
    assert_eq!(
        eval("[{ id = 1, at = sql(\"now()\") }]"),
        list(vec![obj(&[
            ("id", n(1)),
            ("at", Value::Sql("now()".into()))
        ])])
    );
    assert_eq!(
        eval_err("sql(1)"),
        ["`sql`: argument 1 must be a string, found number"]
    );
}

#[test]
fn templates() {
    let doc = load_ok(
        r#"
locals {
  n     = 3
  ok    = true
  items = ["a", "b"]
}
grant {
  plain   = "x-${local.n}-${local.ok}-${"s"}"
  escaped = "$${not} %%{if}"
  if      = "%{ if local.ok }yes%{ else }no%{ endif }"
  for     = "%{ for i, v in local.items }${i}=${v};%{ endfor }"
  strip   = "a  %{~ if true ~}  b  %{~ endif ~}  c"
  lines   = <<-EOT
    %{~ for v in local.items ~}
    - ${v}
    %{~ endfor ~}
    EOT
  else    = "[%{ if false }x%{ else ~}  y  %{~ endif }]"
  nested  = "${upper("${local.items[0]}x")}"
}
"#,
    );
    assert_eq!(attr(&doc, "grant", "plain"), s("x-3-true-s"));
    assert_eq!(attr(&doc, "grant", "escaped"), s("${not} %{if}"));
    assert_eq!(attr(&doc, "grant", "if"), s("yes"));
    assert_eq!(attr(&doc, "grant", "for"), s("0=a;1=b;"));
    assert_eq!(attr(&doc, "grant", "strip"), s("abc"));
    assert_eq!(attr(&doc, "grant", "lines"), s("- a- b"));
    assert_eq!(attr(&doc, "grant", "else"), s("[y]"));
    assert_eq!(attr(&doc, "grant", "nested"), s("AX"));
}

#[test]
fn template_interpolation_errors() {
    assert_eq!(eval_err("\"${null}\""), ["cannot interpolate null"]);
    assert_eq!(eval_err("\"${[1]}\""), ["cannot interpolate a list"]);
    assert_eq!(eval_err("\"${{}}\""), ["cannot interpolate an object"]);
    assert_eq!(
        eval_err("\"${text}\""),
        ["cannot interpolate the identifier `text`"]
    );
    assert_eq!(
        eval_err("\"${varchar(1)}\""),
        ["cannot interpolate the call `varchar(...)`"]
    );
    assert_eq!(
        eval_err("\"${sql(\"x\")}\""),
        ["cannot interpolate a `sql()` value"]
    );
    assert_eq!(
        eval_err("\"%{ if 1 }x%{ endif }\""),
        ["the `if` condition must be a bool, found number"]
    );
    assert_eq!(
        eval_err("\"%{ for x in 1 }x%{ endfor }\""),
        ["a `for` directive needs a list or an object, found number"]
    );
}

#[test]
fn heredocs() {
    let doc = load_ok(
        "grant {\n  indented = <<-SQL\n    SELECT 1\n      FROM t;\n    SQL\n  raw = <<EOT\n  keep\nEOT\n  escaped = <<-SQL\n    $${x} %%{y} ${upper(\"z\")}\n    SQL\n  call = sql(<<-SQL\n    now()\n    SQL\n  )\n  empty = <<-EOT\n  EOT\n}\n",
    );
    assert_eq!(attr(&doc, "grant", "indented"), s("SELECT 1\n  FROM t;\n"));
    assert_eq!(attr(&doc, "grant", "raw"), s("  keep\n"));
    assert_eq!(attr(&doc, "grant", "escaped"), s("${x} %{y} Z\n"));
    assert_eq!(attr(&doc, "grant", "empty"), s(""));
    assert_eq!(attr(&doc, "grant", "call"), Value::Sql("now()\n".into()));
}

#[test]
fn arithmetic_and_comparison() {
    assert_eq!(eval("1 + 2 * 3"), n(7));
    assert_eq!(eval("(1 + 2) * 3"), n(9));
    assert_eq!(eval("7 / 2"), Value::Number(Number::Float(3.5)));
    assert_eq!(eval("8 / 2"), n(4));
    assert_eq!(eval("7 % 3"), n(1));
    assert_eq!(eval("-(2 - 5)"), n(3));
    assert_eq!(eval("1.5 + 1.5"), n(3));
    assert_eq!(eval("2 < 3"), Value::Bool(true));
    assert_eq!(eval("2 >= 3"), Value::Bool(false));
    assert_eq!(eval("\"a\" == \"a\""), Value::Bool(true));
    assert_eq!(eval("null != null"), Value::Bool(false));
    assert_eq!(eval("1 == 1.0"), Value::Bool(true));
    assert_eq!(eval("[1, \"a\"] == [1, \"a\"]"), Value::Bool(true));
    assert_eq!(eval("ROW == ROW"), Value::Bool(true));
    assert_eq!(eval("ROW == STATEMENT"), Value::Bool(false));
    assert_eq!(eval("!false && (true || false)"), Value::Bool(true));
}

#[test]
fn operator_errors() {
    assert_eq!(
        eval_err("1 + \"a\""),
        ["`+` needs numbers, found number and string"]
    );
    assert_eq!(eval_err("1 / 0"), ["division by zero"]);
    assert_eq!(
        eval_err("text < 1"),
        ["`<` needs numbers, found identifier and number"]
    );
    assert_eq!(
        eval_err("varchar(1) == varchar(1)"),
        ["cannot compare with `==`: call and call"]
    );
    assert_eq!(
        eval_err("ROW == \"ROW\""),
        ["cannot compare with `==`: identifier and string"]
    );
    assert_eq!(eval_err("-true"), ["`-` needs a number, found bool"]);
    assert_eq!(eval_err("!1"), ["`!` needs a bool, found number"]);
    assert_eq!(eval_err("1 && true"), ["`&&` needs bools, found number"]);
}

#[test]
fn logical_operators_short_circuit() {
    assert_eq!(eval("false && var.missing"), Value::Bool(false));
    assert_eq!(eval("true || var.missing"), Value::Bool(true));
    assert_eq!(
        eval_err("true && var.missing"),
        ["unknown variable `missing`"]
    );
}

#[test]
fn conditionals_evaluate_only_the_chosen_branch() {
    assert_eq!(eval("true ? 1 : var.missing"), n(1));
    assert_eq!(eval("false ? 1 / 0 : \"b\""), s("b"));
    assert_eq!(
        eval("1 > 2 ? text : varchar(10)"),
        call("varchar", vec![n(10)])
    );
    assert_eq!(
        eval_err("1 ? 1 : 2"),
        ["the condition must be a bool, found number"]
    );
    assert_eq!(
        eval_err("false ? 1 : var.missing"),
        ["unknown variable `missing`"]
    );
}

#[test]
fn for_expressions() {
    assert_eq!(
        eval("[for x in [1, 2, 3] : x * 2]"),
        list(vec![n(2), n(4), n(6)])
    );
    assert_eq!(
        eval("[for x in [1, 2, 3] : x if x != 2]"),
        list(vec![n(1), n(3)])
    );
    assert_eq!(
        eval("[for i, x in [\"a\", \"b\"] : \"${i}${x}\"]"),
        list(vec![s("0a"), s("1b")])
    );
    assert_eq!(
        eval("[for k, v in { a = 1, b = 2 } : \"${k}=${v}\"]"),
        list(vec![s("a=1"), s("b=2")])
    );
    assert_eq!(
        eval("{ for x in [\"a\", \"b\"] : x => upper(x) }"),
        obj(&[("a", s("A")), ("b", s("B"))])
    );
    assert_eq!(
        eval(
            "{ for x in [\"a1\", \"a2\", \"b1\"] : trimsuffix(trimsuffix(x, \"1\"), \"2\") => x... }"
        ),
        obj(&[
            ("a", list(vec![s("a1"), s("a2")])),
            ("b", list(vec![s("b1")]))
        ])
    );
    assert_eq!(eval("[for x in [] : x]"), list(vec![]));
}

#[test]
fn for_expression_errors() {
    assert_eq!(
        eval_err("{ for x in [\"a\", \"a\"] : x => x }"),
        ["duplicate key `a` in `for` expression"]
    );
    assert_eq!(
        eval_err("[for x in 3 : x]"),
        ["a `for` expression needs a list or an object, found number"]
    );
    assert_eq!(
        eval_err("[for x in [1] : x if 1]"),
        ["the `if` clause must be a bool, found number"]
    );
}

#[test]
fn indexing() {
    assert_eq!(eval("[1, 2, 3][1]"), n(2));
    assert_eq!(eval("{ a = { b = [10, 20] } }.a.b[1]"), n(20));
    assert_eq!(eval("{ a = 1 }[\"a\"]"), n(1));
    assert_eq!(
        eval_err("[1][5]"),
        ["index 5 is out of range for a list of 1 elements"]
    );
    assert_eq!(
        eval_err("[1][\"x\"]"),
        ["a list is indexed by a whole number, not `[\"x\"]`"]
    );
    assert_eq!(eval_err("\"abc\"[0]"), ["cannot access `[0]` on a string"]);
    assert_eq!(eval_err("null.x"), ["cannot access `.x` on null"]);
}

#[test]
fn unsupported_syntax() {
    assert_eq!(
        eval_err("[{ a = 1 }][*].a"),
        ["splat expressions are not supported"]
    );
    assert_eq!(
        eval_err("[{ a = 1 }].*.a"),
        ["splat expressions are not supported"]
    );
    assert_eq!(
        eval_err("provider::fn(1)"),
        ["namespaced functions are not supported"]
    );
    assert_eq!(
        eval_err("varchar([1]...)"),
        ["`...` expansion is only supported for HCL functions, not `varchar`"]
    );
}

#[test]
fn unknown_roots_with_segments() {
    assert_eq!(
        eval_err("foo.bar"),
        ["unknown variable or block kind `foo`"]
    );
    let rendered = load_err("grant {\n  v = rol.web\n}\n");
    assert!(rendered.contains("did you mean `role`?"), "{rendered}");
}

#[test]
fn errors_drop_only_their_attribute() {
    let err = load_files(&[(
        "main.hcl",
        "grant {\n  a = 1 + true\n  b = [var.x, local.y]\n  c = 1\n}\n",
    )])
    .unwrap_err();
    let messages: Vec<&str> = err
        .diagnostics
        .items
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "`+` needs numbers, found number and bool",
            "unknown variable `x`",
            "unknown local value `y`"
        ]
    );
}
