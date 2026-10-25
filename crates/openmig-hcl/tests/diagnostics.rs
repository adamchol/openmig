//! Rendered diagnostics for representative errors, so messages stay readable.

mod common;

use common::*;

fn render(files: &[(&str, &str)]) -> String {
    errors(load_files(files))
}

#[test]
fn unknown_block_kind() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        "tabel \"app.users\" {\n  column \"id\" {}\n}\n"
    )]));
}

#[test]
fn duplicate_in_one_file() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        "role \"web\" {}\n\nrole \"web\" {\n  login = true\n}\n"
    )]));
}

#[test]
fn duplicate_across_files() {
    insta::assert_snapshot!(render(&[
        ("schema/a.hcl", "table \"app.users\" {}\n"),
        ("schema/b.hcl", "\ntable \"app.users\" {}\n"),
    ]));
}

#[test]
fn ambiguous_reference() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        r#"table "app.users" {}
table "audit.users" {}
grant {
  on = [table.users]
}
"#
    )]));
}

#[test]
fn unknown_reference() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        r#"table "app.users" {
  column "email" {}
  index "users_email_idx" {
    columns = [column.emial]
  }
}
grant {
  on = [table.app.user]
}
"#
    )]));
}

#[test]
fn missing_variable() {
    insta::assert_snapshot!(render(&[(
        "schema/vars.hcl",
        "variable \"shop_db\" {\n  type = string\n}\n"
    )]));
}

#[test]
fn local_cycle() {
    insta::assert_snapshot!(render(&[(
        "schema/locals.hcl",
        "locals {\n  a = local.b\n  b = \"${local.a}x\"\n}\n"
    )]));
}

#[test]
fn syntax_error() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        "role \"web\" {\n  login = true\n  member_of = [role.a,\n}\n"
    )]));
}

#[test]
fn function_argument_error() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        "grant {\n  privileges = split(\",\", [\"SELECT\"])\n}\n"
    )]));
}

#[test]
fn interpolated_reference() {
    insta::assert_snapshot!(render(&[(
        "schema/main.hcl",
        "table \"app.users\" {}\ntable \"app.archive\" {\n  comment = \"copy of ${table.app.users}\"\n}\n"
    )]));
}
