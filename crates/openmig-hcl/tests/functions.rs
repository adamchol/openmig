//! Built-in functions as seen from schema files.

mod common;

use common::*;
use openmig_hcl::{LoadOptions, Value, load};
use pretty_assertions::assert_eq;

#[test]
fn every_function() {
    let cases: &[(&str, Value)] = &[
        ("format(\"%s_%03d\", \"t\", 7)", s("t_007")),
        ("join(\", \", [\"a\", \"b\"])", s("a, b")),
        (
            "split(\".\", \"app.users\")",
            list(vec![s("app"), s("users")]),
        ),
        ("lower(\"ABC\")", s("abc")),
        ("upper(\"abc\")", s("ABC")),
        ("replace(\"a-b-c\", \"-\", \"_\")", s("a_b_c")),
        ("trimspace(\"  x  \")", s("x")),
        ("trimprefix(\"app_users\", \"app_\")", s("users")),
        ("trimsuffix(\"users_old\", \"_old\")", s("users")),
        ("chomp(\"x\\n\")", s("x")),
        ("concat([1], [2, 3])", list(vec![n(1), n(2), n(3)])),
        ("flatten([[1], [[2]]])", list(vec![n(1), n(2)])),
        ("distinct([1, 1, 2])", list(vec![n(1), n(2)])),
        ("contains([\"a\"], \"a\")", Value::Bool(true)),
        ("keys({ b = 1, a = 2 })", list(vec![s("b"), s("a")])),
        ("values({ b = 1, a = 2 })", list(vec![n(1), n(2)])),
        (
            "merge({ a = 1 }, { b = 2 })",
            obj(&[("a", n(1)), ("b", n(2))]),
        ),
        ("coalesce(null, \"\", \"x\")", s("x")),
        ("length(\"abc\")", n(3)),
        ("range(3)", list(vec![n(0), n(1), n(2)])),
        ("range(1, 3)", list(vec![n(1), n(2)])),
        ("range(0, 6, 3)", list(vec![n(0), n(3)])),
        ("tostring(5)", s("5")),
        ("tonumber(\"5\")", n(5)),
        ("tobool(\"true\")", Value::Bool(true)),
        (
            "try(var.missing, local.missing, \"fallback\")",
            s("fallback"),
        ),
        ("try(1 / 0, 2)", n(2)),
        ("try(\"first\", var.missing)", s("first")),
        ("jsonencode({ a = [1, true] })", s(r#"{"a":[1,true]}"#)),
        (
            "jsondecode(\"{\\\"a\\\": [1, null]}\")",
            obj(&[("a", list(vec![n(1), Value::Null]))]),
        ),
        ("sql(\"now()\")", Value::Sql("now()".into())),
        ("concat([[1], [2]]...)", list(vec![n(1), n(2)])),
        ("format(\"%s-%s\", [\"a\", \"b\"]...)", s("a-b")),
    ];
    for (expr, expected) in cases {
        assert_eq!(&eval(expr), expected, "{expr}");
    }
}

#[test]
fn try_reports_when_every_argument_fails() {
    let rendered = load_err("grant {\n  v = try(var.a, var.b)\n}\n");
    assert!(
        rendered.contains("no argument of `try` could be evaluated"),
        "{rendered}"
    );
    assert!(
        rendered.contains("the last argument failed with: unknown variable `b`"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("×").count(), 1, "{rendered}");
}

#[test]
fn arity_and_type_errors_point_at_the_call() {
    let err = load_files(&[(
        "main.hcl",
        "grant {\n  v = join(\",\")\n  w = upper([1])\n}\n",
    )])
    .unwrap_err();
    let d = &err.diagnostics.items;
    assert_eq!(d[0].message, "`join` takes 2 arguments, got 1");
    let src = &err.sources.get(d[0].span.unwrap().file).content;
    let span = d[0].span.unwrap();
    assert_eq!(&src[span.start..span.end], "join(\",\")");
    assert_eq!(
        d[1].message,
        "`upper`: argument 1 must be a string, found list"
    );
    let span = d[1].span.unwrap();
    assert_eq!(&src[span.start..span.end], "[1]");
}

#[test]
fn file_is_relative_to_the_calling_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub/sql")).unwrap();
    std::fs::write(dir.path().join("sub/sql/body.sql"), "SELECT 1;\n").unwrap();
    std::fs::write(
        dir.path().join("sub/fn.hcl"),
        "function \"f\" {\n  body = file(\"sql/body.sql\")\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("bad.hcl"),
        "function \"g\" {\n  body = file(\"sql/body.sql\")\n}\n",
    )
    .unwrap();
    let spec = spec();
    let opts = LoadOptions {
        spec: &spec,
        vars: Default::default(),
    };
    let err = load(&[dir.path().to_path_buf()], &opts).unwrap_err();
    assert_eq!(err.diagnostics.items.len(), 1);
    assert!(err.diagnostics.items[0].message.starts_with("cannot read"));

    std::fs::remove_file(dir.path().join("bad.hcl")).unwrap();
    let doc = load(&[dir.path().to_path_buf()], &opts).unwrap();
    assert_eq!(attr(&doc, "function", "body"), s("SELECT 1;\n"));
}
