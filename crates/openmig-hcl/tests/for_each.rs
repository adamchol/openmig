//! `for_each` expansion at the top level and in nested blocks.

mod common;

use common::*;
use pretty_assertions::assert_eq;

#[test]
fn lists_create_one_instance_per_element() {
    let doc = load_ok(
        r#"
grant {
  for_each   = ["web", "worker"]
  privileges = [SELECT]
  to         = [each.value]
  index      = each.key
}
"#,
    );
    assert_eq!(doc.blocks.len(), 2);
    let keys: Vec<Option<&str>> = doc.blocks.iter().map(|b| b.each_key.as_deref()).collect();
    assert_eq!(keys, [Some("0"), Some("1")]);
    assert_eq!(block_attr(&doc.blocks[1], "to"), list(vec![s("worker")]));
    assert_eq!(block_attr(&doc.blocks[1], "index"), n(1));
    assert!(doc.blocks[0].attr("for_each").is_none());
}

#[test]
fn objects_create_one_instance_per_key() {
    let doc = load_ok(
        r#"
locals {
  roles = { web = true, worker = false }
}
role "r" {
  for_each = local.roles
  name     = "app_${each.key}"
  login    = each.value
}
"#,
    );
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(doc.blocks[0].each_key.as_deref(), Some("web"));
    assert_eq!(doc.blocks[0].address, None);
    assert_eq!(doc.blocks[0].labels, ["r"]);
    assert_eq!(block_attr(&doc.blocks[0], "name"), s("app_web"));
    assert_eq!(
        block_attr(&doc.blocks[1], "login"),
        openmig_hcl::Value::Bool(false)
    );
}

#[test]
fn instances_do_not_conflict_and_cannot_be_referenced() {
    let rendered = load_err(
        r#"
role "r" {
  for_each = ["a", "b"]
}
role "r" {}
grant {
  to = [role.x]
}
"#,
    );
    assert!(!rendered.contains("declared more than once"), "{rendered}");
    let rendered = load_err("role \"r\" {\n  for_each = [\"a\"]\n}\ngrant {\n  to = [role.r]\n}\n");
    assert!(rendered.contains("unknown role `r`"), "{rendered}");
    assert!(
        rendered.contains("uses `for_each`; its instances cannot be referenced"),
        "{rendered}"
    );
}

#[test]
fn nested_for_each() {
    let doc = load_ok(
        r#"
locals {
  columns = { id = bigint, name = text }
}
table "app.t" {
  column "c" {
    for_each = local.columns
    name     = each.key
    type     = each.value
  }
  column "extra" {}
  index "i" {
    key {
      for_each = ["a", "b"]
      expr     = "lower(${each.value})"
    }
  }
}
"#,
    );
    let table = &doc.blocks[0];
    assert_eq!(table.address, Some(addr("table", &["app", "t"], &[])));
    let columns: Vec<(Option<&str>, openmig_hcl::Value)> = table
        .children("column")
        .map(|c| {
            (
                c.each_key.as_deref(),
                c.attr("type")
                    .map(|a| a.value.clone())
                    .unwrap_or(openmig_hcl::Value::Null),
            )
        })
        .collect();
    assert_eq!(
        columns,
        [
            (Some("id"), ident("bigint")),
            (Some("name"), ident("text")),
            (None, openmig_hcl::Value::Null),
        ]
    );
    assert_eq!(table.blocks[0].address, None);
    assert_eq!(
        table.blocks[2].address,
        Some(addr("table", &["app", "t"], &[("column", "extra")]))
    );
    let keys: Vec<openmig_hcl::Value> = table.blocks[3]
        .children("key")
        .map(|k| block_attr(k, "expr"))
        .collect();
    assert_eq!(keys, [s("lower(a)"), s("lower(b)")]);
}

#[test]
fn nested_blocks_see_the_instance() {
    let doc = load_ok(
        r#"
table "t" {
  for_each = { a = "x", b = "y" }
  name     = each.key
  column "c" {
    comment = "${each.key}:${each.value}"
  }
}
"#,
    );
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(block_attr(&doc.blocks[1].blocks[0], "comment"), s("b:y"));
    assert_eq!(doc.blocks[1].blocks[0].address, None);
    assert_eq!(doc.blocks[1].blocks[0].each_key, None);
}

#[test]
fn inner_for_each_shadows_outer() {
    let doc = load_ok(
        r#"
table "t" {
  for_each = ["outer"]
  column "c" {
    for_each = [each.value]
    comment  = "${each.key}:${each.value}"
  }
}
"#,
    );
    assert_eq!(
        block_attr(&doc.blocks[0].blocks[0], "comment"),
        s("0:outer")
    );
}

#[test]
fn relative_references_inside_instances_are_errors() {
    let rendered = load_err(
        "table \"t\" {\n  for_each = [1]\n  column \"id\" {}\n  primary_key {\n    columns = [column.id]\n  }\n}\n",
    );
    assert!(
        rendered.contains(
            "cannot reference `column.id` here: the enclosing `table` block has no address"
        ),
        "{rendered}"
    );
}

#[test]
fn for_each_errors() {
    assert_eq!(
        err_messages("grant {\n  for_each = 3\n}\n"),
        ["`for_each` must be a list or an object, found number"]
    );
    assert_eq!(
        err_messages("grant {\n  v = each.value\n}\n"),
        ["`each` is only available in blocks with `for_each`"]
    );
    assert_eq!(
        err_messages("grant {\n  for_each = [1]\n  v = each.other\n}\n"),
        ["`each` has only `each.key` and `each.value`"]
    );
    assert!(load_ok("grant {\n  for_each = []\n}\n").blocks.is_empty());
}
