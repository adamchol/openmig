//! Block references: qualified and unqualified, nested, relative, `.name`, index syntax.

mod common;

use common::*;
use pretty_assertions::assert_eq;

const SCHEMA: &str = r#"
schema "app" {}

table "app.users" {
  column "id" {
    type = bigint
  }
  column "email" {
    type = text
  }
  index "users_email_idx" {
    columns = [column.email]
  }
}

table "audit.users" {
  column "id" {}
}

table "orgs" {
  name = "organisations"
  column "id" {}
}

role "s3-uploader" {}
role "odd.name" {}
"#;

fn refs(expr: &str) -> openmig_hcl::Value {
    let doc = load_files(&[
        ("schema.hcl", SCHEMA),
        ("refs.hcl", &format!("grant {{\n  v = {expr}\n}}\n")),
    ])
    .unwrap_or_else(|e| panic!("{}", e.render(false)));
    attr(&doc, "grant", "v")
}

fn ref_errors(expr: &str) -> Vec<String> {
    messages(load_files(&[
        ("schema.hcl", SCHEMA),
        ("refs.hcl", &format!("grant {{\n  v = {expr}\n}}\n")),
    ]))
}

#[test]
fn qualified_references() {
    assert_eq!(
        refs("table.app.users"),
        reference("table", &["app", "users"], &[])
    );
    assert_eq!(
        refs("table.public.orgs"),
        reference("table", &["public", "orgs"], &[])
    );
    assert_eq!(refs("schema.app"), reference("schema", &["app"], &[]));
}

#[test]
fn unqualified_references_resolve_unique_names() {
    assert_eq!(
        refs("table.orgs"),
        reference("table", &["public", "orgs"], &[])
    );
    assert_eq!(
        refs("table.orgs.column.id"),
        reference("table", &["public", "orgs"], &[("column", "id")])
    );
}

#[test]
fn ambiguous_references_list_candidates() {
    assert_eq!(
        ref_errors("table.users"),
        ["ambiguous reference `table.users`, candidates: table.app.users, table.audit.users"]
    );
}

#[test]
fn unknown_references_suggest_existing_blocks() {
    let err = errors(load_files(&[
        ("schema.hcl", SCHEMA),
        (
            "refs.hcl",
            "grant {\n  v = table.app.userz\n  w = role.s3-uploadr\n}\n",
        ),
    ]));
    assert!(err.contains("unknown table `app.userz`"), "{err}");
    assert!(err.contains("did you mean `table.app.users`?"), "{err}");
    assert!(err.contains("unknown role `s3-uploadr`"), "{err}");
    assert!(err.contains("did you mean `role.s3-uploader`?"), "{err}");
    assert_eq!(ref_errors("table.nothing"), ["unknown table `nothing`"]);
}

#[test]
fn nested_references() {
    assert_eq!(
        refs("table.app.users.column.id"),
        reference("table", &["app", "users"], &[("column", "id")])
    );
    assert_eq!(
        refs("table.app.users.index.users_email_idx"),
        reference("table", &["app", "users"], &[("index", "users_email_idx")])
    );
    assert_eq!(
        ref_errors("table.app.users.column.idd"),
        ["unknown column `idd` in `table.app.users`"]
    );
    assert_eq!(
        ref_errors("table.app.users.primary_key"),
        ["unexpected `.primary_key` after `table.app.users`"]
    );
    assert_eq!(
        ref_errors("table.app.users.column"),
        ["expected a label after `.column`"]
    );
}

#[test]
fn relative_references_use_the_enclosing_block() {
    let doc = load_ok(
        r#"
table "app.users" {
  column "id" {}
  column "email" {}
  primary_key {
    columns = [column.id]
  }
  index "users_email_idx" {
    key {
      column = column.email
    }
  }
  foreign_key "self" {
    columns    = [column.id]
    references = [table.app.users.column.id]
  }
  replica_identity = index.users_email_idx
}
"#,
    );
    let table = &doc.blocks[0];
    let id = reference("table", &["app", "users"], &[("column", "id")]);
    assert_eq!(
        block_attr(&table.blocks[2], "columns"),
        list(vec![id.clone()])
    );
    assert_eq!(
        block_attr(&table.blocks[3].blocks[0], "column"),
        reference("table", &["app", "users"], &[("column", "email")])
    );
    assert_eq!(block_attr(&table.blocks[4], "references"), list(vec![id]));
    assert_eq!(
        block_attr(table, "replica_identity"),
        reference("table", &["app", "users"], &[("index", "users_email_idx")])
    );
}

#[test]
fn relative_references_must_exist() {
    let messages = err_messages(
        "table \"t\" {\n  column \"id\" {}\n  primary_key {\n    columns = [column.idx]\n  }\n}\n",
    );
    assert_eq!(messages, ["unknown column `idx` in `table.public.t`"]);
    let rendered = load_err(
        "table \"t\" {\n  column \"id\" {}\n  primary_key {\n    columns = [column.idx]\n  }\n}\n",
    );
    assert!(rendered.contains("did you mean `column.id`?"), "{rendered}");
}

#[test]
fn bare_kinds_outside_their_parent_are_identifiers() {
    assert_eq!(refs("column"), ident("column"));
    assert_eq!(refs("table"), ident("table"));
    assert_eq!(
        ref_errors("column.id"),
        ["unknown variable or block kind `column`"]
    );
}

#[test]
fn name_attribute() {
    assert_eq!(refs("table.app.users.name"), s("users"));
    assert_eq!(refs("table.orgs.name"), s("organisations"));
    assert_eq!(
        refs("\"${table.app.users.name}_archive\""),
        s("users_archive")
    );
    assert_eq!(refs("table.app.users.column.email.name"), s("email"));
    assert_eq!(refs("role.s3-uploader.name"), s("s3-uploader"));
}

#[test]
fn name_can_come_from_variables_and_locals() {
    let doc = load_files_with(
        &[(
            "main.hcl",
            r#"
variable "db" {
  type = string
}
locals {
  prefix = "x_"
}
schema "app" {
  name = var.db
}
table "app.t" {
  name = "${local.prefix}${schema.app.name}"
}
grant {
  v = table.app.t.name
}
"#,
        )],
        &[("db", openmig_hcl::VarValue::Raw("dialo".into()))],
    )
    .unwrap();
    assert_eq!(attr(&doc, "schema", "name"), s("dialo"));
    assert_eq!(attr(&doc, "grant", "v"), s("x_dialo"));
}

#[test]
fn non_string_names_fall_back_to_the_label() {
    let doc = load_ok("role \"web\" {\n  name = null\n}\ngrant {\n  v = role.web.name\n}\n");
    assert_eq!(attr(&doc, "grant", "v"), s("web"));
}

#[test]
fn name_cycles_are_errors() {
    let messages = err_messages(
        "role \"a\" {\n  name = role.b.name\n}\nrole \"b\" {\n  name = role.a.name\n}\n",
    );
    assert_eq!(messages, ["`role.a.name` depends on itself"]);
}

#[test]
fn hyphenated_and_index_syntax() {
    assert_eq!(
        refs("role.s3-uploader"),
        reference("role", &["s3-uploader"], &[])
    );
    assert_eq!(
        refs("role[\"odd.name\"]"),
        reference("role", &["odd.name"], &[])
    );
    assert_eq!(
        refs("table[\"app\"][\"users\"]"),
        reference("table", &["app", "users"], &[])
    );
    assert_eq!(
        refs("table.app[\"users\"].column[\"id\"]"),
        reference("table", &["app", "users"], &[("column", "id")])
    );
}

#[test]
fn references_survive_locals_and_indexing() {
    let doc = load_files(&[
        ("schema.hcl", SCHEMA),
        (
            "refs.hcl",
            r#"
locals {
  tables = [table.app.users, table.orgs]
  owner  = local.tables[0]
}
grant {
  a = local.owner
  b = local.owner.column.id
  c = local.tables[1].name
  d = [for t in local.tables : t.name]
  e = local.tables[0] == table.app.users
}
"#,
        ),
    ])
    .unwrap();
    assert_eq!(
        attr(&doc, "grant", "a"),
        reference("table", &["app", "users"], &[])
    );
    assert_eq!(
        attr(&doc, "grant", "b"),
        reference("table", &["app", "users"], &[("column", "id")])
    );
    assert_eq!(attr(&doc, "grant", "c"), s("organisations"));
    assert_eq!(
        attr(&doc, "grant", "d"),
        list(vec![s("users"), s("organisations")])
    );
    assert_eq!(attr(&doc, "grant", "e"), openmig_hcl::Value::Bool(true));
}

#[test]
fn forward_references_across_files() {
    let doc = load_files(&[
        ("a.hcl", "table \"app.orders\" {\n  column \"user_id\" {}\n  foreign_key \"fk\" {\n    columns    = [column.user_id]\n    references = [table.app.users.column.id]\n  }\n}\n"),
        ("b.hcl", "table \"app.users\" {\n  column \"id\" {}\n}\n"),
    ])
    .unwrap();
    let fk = &doc.blocks[0].blocks[1];
    assert_eq!(
        block_attr(fk, "references"),
        list(vec![reference(
            "table",
            &["app", "users"],
            &[("column", "id")]
        )])
    );
}

#[test]
fn non_referenceable_kinds() {
    assert_eq!(eval_err("grant.x"), ["`grant` blocks cannot be referenced"]);
    assert_eq!(
        eval_err("foo.bar"),
        ["unknown variable or block kind `foo`"]
    );
}

#[test]
fn references_cannot_be_interpolated() {
    let rendered = errors(load_files(&[
        ("schema.hcl", SCHEMA),
        ("refs.hcl", "grant {\n  v = \"${table.app.users}\"\n}\n"),
    ]));
    assert!(
        rendered.contains("cannot interpolate the reference `table.app.users`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("use `table.app.users.name`"),
        "{rendered}"
    );
}
