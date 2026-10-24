//! Block structure: kinds, labels, addresses, duplicates, spans and file handling.

mod common;

use common::*;
use openmig_hcl::{LoadOptions, SourceMap, load};
use pretty_assertions::assert_eq;

#[test]
fn qualified_labels_get_the_default_namespace() {
    let doc = load_ok(
        r#"
table "app.users" {}
table "events" {}
table "app.odd.name" {}
"#,
    );
    let addresses: Vec<String> = doc
        .blocks
        .iter()
        .map(|b| b.address.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        addresses,
        [
            "table.app.users",
            "table.public.events",
            "table.app[\"odd.name\"]"
        ]
    );
    assert_eq!(doc.blocks[1].labels, ["events"]);
    assert_eq!(
        doc.blocks[2].address.as_ref().unwrap().labels,
        ["app", "odd.name"]
    );
}

#[test]
fn identifier_labels_are_accepted() {
    let doc = load_ok("role web {}\n");
    assert_eq!(doc.blocks[0].address, Some(addr("role", &["web"], &[])));
}

#[test]
fn nested_blocks_get_addresses() {
    let doc = load_ok(
        r#"
table "app.users" {
  column "id" {
    type = bigint
  }
  index "users_id_idx" {
    key {
      column = column.id
    }
  }
  index {
    columns = [column.id]
  }
  primary_key {
    columns = [column.id]
  }
}
grant {
  privileges = [SELECT]
}
"#,
    );
    let table = &doc.blocks[0];
    let kinds: Vec<&str> = table.blocks.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(kinds, ["column", "index", "index", "primary_key"]);
    assert_eq!(
        table.blocks[0].address,
        Some(addr("table", &["app", "users"], &[("column", "id")]))
    );
    assert_eq!(
        table.blocks[1].address,
        Some(addr(
            "table",
            &["app", "users"],
            &[("index", "users_id_idx")]
        ))
    );
    assert_eq!(table.blocks[1].blocks[0].address, None);
    assert_eq!(table.blocks[2].address, None);
    assert_eq!(table.blocks[3].address, None);
    assert_eq!(doc.blocks[1].address, None);
    let column = doc
        .get(&addr("table", &["app", "users"], &[("column", "id")]))
        .unwrap();
    assert_eq!(block_attr(column, "type"), ident("bigint"));
}

#[test]
fn attributes_keep_source_order_and_spans() {
    let src = "role \"web\" {\n  login = true\n  comment = \"x\"\n}\n";
    let doc = load_ok(src);
    let block = &doc.blocks[0];
    let names: Vec<&str> = block.attrs.keys().map(String::as_str).collect();
    assert_eq!(names, ["login", "comment"]);
    let login = block.attr("login").unwrap();
    assert_eq!(&src[login.span.start..login.span.end], "login = true");
    assert_eq!(&src[login.value_span.start..login.value_span.end], "true");
    assert_eq!(
        &src[block.header_span.start..block.header_span.end],
        "role \"web\""
    );
    assert_eq!(&src[block.span.start..block.span.end], src.trim_end());
    assert_eq!(block.each_key, None);
}

#[test]
fn variables_and_locals_are_not_blocks() {
    let doc = load_ok(
        r#"
variable "x" {
  default = 1
}
locals {
  y = 2
}
role "web" {}
"#,
    );
    let kinds: Vec<&str> = doc.blocks.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(kinds, ["role"]);
    assert!(doc.warnings.is_empty());
}

#[test]
fn files_share_one_namespace_in_order() {
    let doc = load_files(&[
        (
            "a.hcl",
            "table \"app.orders\" {\n  owner = table.app.users\n}\n",
        ),
        ("b.hcl", "table \"app.users\" {}\n"),
    ])
    .unwrap();
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(
        block_attr(&doc.blocks[0], "owner"),
        reference("table", &["app", "users"], &[])
    );
    assert_eq!(
        doc.sources.path(doc.blocks[1].span.file).to_str(),
        Some("b.hcl")
    );
}

#[test]
fn duplicates_are_errors_with_both_spans() {
    let result = load_files(&[
        ("a.hcl", "table \"app.users\" {}\n"),
        (
            "b.hcl",
            "role \"web\" {}\ntable \"app.users\" {}\nrole \"web\" {}\n",
        ),
    ]);
    let err = result.unwrap_err();
    let messages: Vec<&str> = err
        .diagnostics
        .items
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "`table.app.users` is declared more than once",
            "`role.web` is declared more than once"
        ]
    );
    let first = &err.diagnostics.items[0];
    assert_eq!(first.related.len(), 1);
    assert_eq!(
        err.sources.path(first.related[0].0.file).to_str(),
        Some("a.hcl")
    );
    assert_eq!(
        err.sources.path(first.span.unwrap().file).to_str(),
        Some("b.hcl")
    );
}

#[test]
fn qualified_duplicates_compare_canonical_labels() {
    let messages = err_messages("table \"users\" {}\ntable \"public.users\" {}\n");
    assert_eq!(
        messages,
        ["`table.public.users` is declared more than once"]
    );
}

#[test]
fn nested_duplicates_are_errors() {
    let messages = err_messages(
        "table \"t\" {\n  column \"id\" {}\n  column \"id\" {}\n}\ntable \"u\" {\n  column \"id\" {}\n}\n",
    );
    assert_eq!(
        messages,
        ["`table.public.t.column.id` is declared more than once"]
    );
}

#[test]
fn mergeable_blocks_are_all_kept() {
    let doc = load_ok(
        r#"
data "app.statuses" {
  rows = [{ id = 1 }]
}
data "app.statuses" {
  rows = [{ id = 2 }]
}
"#,
    );
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(doc.blocks[0].address, doc.blocks[1].address);
}

#[test]
fn unknown_block_kinds() {
    let messages = err_messages(
        r#"
tabel "x" {}
table "y" {
  colum "id" {}
  column "ok" {
    nested {}
  }
}
"#,
    );
    assert_eq!(
        messages,
        [
            "unknown block kind `tabel`",
            "unknown block kind `colum` inside `table`",
            "unknown block kind `nested` inside `column`",
        ]
    );
    let rendered = load_err("tabel \"x\" {}\n");
    assert!(rendered.contains("did you mean `table`?"), "{rendered}");
}

#[test]
fn label_rules() {
    let messages = err_messages(
        r#"
role {}
role "a" "b" {}
grant "x" {}
table "app." {}
table ".users" {}
role "" {}
"#,
    );
    assert_eq!(
        messages,
        [
            "`role` blocks need a label",
            "`role` blocks take one label, found 2",
            "`grant` blocks take no labels",
            "invalid label `app.`",
            "invalid label `.users`",
            "labels cannot be empty",
        ]
    );
}

#[test]
fn optional_labels() {
    let doc = load_ok("function \"f\" {\n  arg {}\n  arg \"x\" {}\n}\n");
    let args = &doc.blocks[0].blocks;
    assert_eq!(args[0].labels, Vec::<String>::new());
    assert_eq!(args[1].labels, ["x"]);
}

#[test]
fn top_level_attributes_are_errors() {
    let messages = err_messages("x = 1\nrole \"web\" {}\n");
    assert_eq!(messages, ["unexpected attribute `x` outside of a block"]);
}

#[test]
fn parse_errors_point_at_the_right_line() {
    let err = load_files(&[
        ("ok.hcl", "role \"a\" {}\n"),
        (
            "bad.hcl",
            "role \"web\" {\n  login = true\n  comment = [1,\n}\n",
        ),
    ])
    .unwrap_err();
    assert_eq!(err.diagnostics.items.len(), 1);
    let d = &err.diagnostics.items[0];
    assert!(d.message.starts_with("syntax error: "), "{}", d.message);
    let span = d.span.unwrap();
    assert_eq!(err.sources.path(span.file).to_str(), Some("bad.hcl"));
    assert_eq!(err.sources.line_col(span.file, span.start), (4, 1));
}

#[test]
fn errors_in_several_files_are_all_reported() {
    let messages = messages(load_files(&[
        ("a.hcl", "role \"a\" {\n  x = var.missing\n}\n"),
        ("b.hcl", "role \"b\" {\n  x = 1 +\n"),
        ("c.hcl", "tabel \"c\" {}\n"),
    ]));
    assert_eq!(messages.len(), 3, "{messages:?}");
}

#[test]
fn load_walks_directories_in_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("b/nested")).unwrap();
    std::fs::write(root.join("b/nested/z.hcl"), "role \"z\" {}\n").unwrap();
    std::fs::write(root.join("b/a.hcl"), "role \"ba\" {}\n").unwrap();
    std::fs::write(root.join("a.hcl"), "role \"a\" {}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "not hcl").unwrap();
    let overlay = dir.path().join("overlay.conf");
    std::fs::write(&overlay, "role \"overlay\" {}\n").unwrap();
    let spec = spec();
    let opts = LoadOptions {
        spec: &spec,
        vars: Default::default(),
    };
    let doc = load(&[root.to_path_buf(), overlay.clone()], &opts).unwrap();
    let labels: Vec<&str> = doc.blocks.iter().map(|b| b.labels[0].as_str()).collect();
    assert_eq!(labels, ["a", "ba", "z", "overlay"]);

    let missing = load(&[root.join("missing")], &opts).unwrap_err();
    assert!(
        missing.diagnostics.items[0]
            .message
            .starts_with("cannot read")
    );
}

#[test]
fn load_error_renders_with_sources() {
    let err = load_files(&[("main.hcl", "tabel \"x\" {}\n")]).unwrap_err();
    assert_eq!(err.to_string(), err.render(false));
    let _: &SourceMap = &err.sources;
}
