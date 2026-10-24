//! The canonical HCL writer: formatting rules and exact round trips through the loader.

use indexmap::IndexMap;
use openmig_hcl::write::{WBlock, WExpr, WItem, render, render_expr};
use openmig_hcl::{
    Address, Block, BlockSpec, LabelSpec, LoadOptions, Number, SchemaSpec, Value, load_sources,
};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

static ITEM: [BlockSpec; 1] = [BlockSpec {
    kind: "item",
    labels: LabelSpec::OptionalOne,
    referenceable: false,
    mergeable: true,
    nested: &ITEM,
}];

static MEMBER: &[BlockSpec] = &[BlockSpec::new("member", LabelSpec::One).referenceable()];

static BLOCKS: &[BlockSpec] = &[
    ITEM[0],
    BlockSpec::new("role", LabelSpec::One)
        .referenceable()
        .nested(MEMBER),
    BlockSpec::new("table", LabelSpec::Qualified).referenceable(),
];

const TARGETS: &str = r#"
role "r0" {
  member "m0" {}
}
role "odd.name" {}
table "app.users" {}
"#;

fn s(v: &str) -> WExpr {
    WExpr::String(v.to_owned())
}

fn num(v: &str) -> WExpr {
    WExpr::Number(v.to_owned())
}

fn id(v: &str) -> WExpr {
    WExpr::Ident(v.to_owned())
}

fn address(kind: &str, labels: &[&str], nested: &[(&str, &str)]) -> Address {
    let mut a = Address::new(kind, labels.iter().map(|l| (*l).to_owned()).collect());
    for (k, l) in nested {
        a = a.child(*k, *l);
    }
    a
}

/// Renders `blocks` and loads the result together with the reference targets.
fn reload(blocks: &[WBlock]) -> (String, Vec<Block>) {
    let text = render(blocks);
    let spec = SchemaSpec {
        blocks: BLOCKS,
        default_namespace: "public".to_owned(),
    };
    let opts = LoadOptions {
        spec: &spec,
        vars: IndexMap::new(),
    };
    let sources = vec![
        (PathBuf::from("targets.hcl"), TARGETS.to_owned()),
        (PathBuf::from("out.hcl"), text.clone()),
    ];
    match load_sources(sources, &opts) {
        Ok(doc) => {
            let blocks = doc
                .blocks
                .into_iter()
                .filter(|b| doc_file(&b.span) == 1)
                .collect();
            (text, blocks)
        }
        Err(e) => panic!("rendered HCL does not load:\n{text}\n{}", e.render(false)),
    }
}

fn doc_file(span: &openmig_hcl::Span) -> u32 {
    span.file.0
}

/// The value the loader must produce for a written expression.
fn expected(expr: &WExpr) -> Value {
    match expr {
        WExpr::Null => Value::Null,
        WExpr::Bool(b) => Value::Bool(*b),
        WExpr::Number(t) => Value::Number(Number::from_literal(t).expect("valid number")),
        WExpr::String(v) => Value::String(v.clone()),
        WExpr::Ident(v) => Value::Ident(v.clone()),
        WExpr::Call(name, args) => Value::Call {
            name: name.clone(),
            args: args.iter().map(expected).collect(),
        },
        WExpr::Ref(a) => Value::Ref(a.clone()),
        WExpr::Sql(v) => Value::Sql(v.clone()),
        WExpr::List(items) => Value::List(items.iter().map(expected).collect()),
        WExpr::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), expected(v)))
                .collect(),
        ),
    }
}

#[derive(Debug, PartialEq)]
struct Shape {
    kind: String,
    labels: Vec<String>,
    attrs: Vec<(String, Value)>,
    blocks: Vec<Shape>,
}

fn written_shape(block: &WBlock) -> Shape {
    Shape {
        kind: block.kind.clone(),
        labels: block.labels.clone(),
        attrs: block
            .items
            .iter()
            .filter_map(|i| match i {
                WItem::Attr(name, value) => Some((name.clone(), expected(value))),
                _ => None,
            })
            .collect(),
        blocks: block
            .items
            .iter()
            .filter_map(|i| match i {
                WItem::Block(b) => Some(written_shape(b)),
                _ => None,
            })
            .collect(),
    }
}

fn loaded_shape(block: &Block) -> Shape {
    Shape {
        kind: block.kind.clone(),
        labels: block.labels.clone(),
        attrs: block
            .attrs
            .values()
            .map(|a| (a.name.clone(), a.value.clone()))
            .collect(),
        blocks: block.blocks.iter().map(loaded_shape).collect(),
    }
}

fn assert_round_trip(blocks: &[WBlock]) -> String {
    let (text, loaded) = reload(blocks);
    let want: Vec<Shape> = blocks.iter().map(written_shape).collect();
    let got: Vec<Shape> = loaded.iter().map(loaded_shape).collect();
    assert_eq!(got, want, "rendered:\n{text}");
    text
}

fn item(items: Vec<WItem>) -> WBlock {
    WBlock {
        kind: "item".to_owned(),
        labels: vec!["x".to_owned()],
        items,
    }
}

fn attr(name: &str, value: WExpr) -> WItem {
    WItem::Attr(name.to_owned(), value)
}

#[test]
fn canonical_layout() {
    let table = WBlock::new("table", vec!["app.users".into()])
        .attr("comment", s("Application users"))
        .attr("renamed_from", s("app.people"))
        .block(
            WBlock::new("column", vec!["id".into()])
                .attr("type", id("bigint"))
                .attr("null", WExpr::Bool(false))
                .attr(
                    "identity",
                    WExpr::Object(vec![("generated".into(), id("ALWAYS"))]),
                ),
        )
        .block(
            WBlock::new("column", vec!["email".into()])
                .attr("type", WExpr::Call("varchar".into(), vec![num("255")]))
                .attr("default", WExpr::Sql("lower('x')".into())),
        )
        .block(WBlock::new("column", vec!["note".into()]))
        .block(WBlock::new("primary_key", vec![]).attr(
            "columns",
            WExpr::List(vec![WExpr::Ref(address(
                "table",
                &["app", "users"],
                &[("column", "id")],
            ))]),
        ));
    let mut function = WBlock::new("function", vec!["app.touch".into()]);
    function
        .items
        .push(WItem::Comment("Keeps updated_at current.".into()));
    function.push_attr("language", id("plpgsql"));
    function.push_attr("returns", id("trigger"));
    function.items.push(WItem::Blank);
    function.push_attr(
        "body",
        s("BEGIN\n  NEW.updated_at = now();\n  RETURN NEW;\nEND;\n"),
    );
    function.push_attr(
        "set",
        WExpr::Object(vec![("search_path".into(), s("pg_catalog"))]),
    );
    let grant = WBlock::new("grant", vec![])
        .attr("privileges", WExpr::List(vec![id("SELECT"), id("INSERT")]))
        .attr(
            "on",
            WExpr::List(vec![WExpr::Ref(address("table", &["app", "users"], &[]))]),
        )
        .attr(
            "to",
            WExpr::List(vec![
                WExpr::Ref(address("role", &["s3-uploader"], &[])),
                id("PUBLIC"),
            ]),
        );
    insta::assert_snapshot!(render(&[table, function, grant]));
}

#[test]
fn empty_blocks_are_one_line() {
    assert_eq!(
        render(&[WBlock::new("role", vec!["web".into()])]),
        "role \"web\" {}\n"
    );
    assert_eq!(
        render(&[WBlock::new("grant", vec![]).block(WBlock::new("key", vec![]))]),
        "grant {\n  key {}\n}\n"
    );
    let blanks = WBlock {
        kind: "grant".into(),
        labels: vec![],
        items: vec![WItem::Blank, WItem::Blank],
    };
    assert_eq!(render(&[blanks]), "grant {}\n");
}

#[test]
fn alignment_runs_break_at_blocks_comments_and_multi_line_values() {
    let block = item(vec![
        attr("a", num("1")),
        attr("long_name", num("2")),
        WItem::Comment("next run".into()),
        attr("bb", num("3")),
        attr("ccc", s("multi\nline\n")),
        attr("d", num("4")),
        attr("eeeee", num("5")),
        WItem::Block(WBlock::new("item", vec![])),
        attr("f", num("6")),
        WItem::Blank,
        WItem::Blank,
        attr("g", num("7")),
        WItem::Blank,
    ]);
    assert_eq!(
        render(&[block]),
        "item \"x\" {
  a         = 1
  long_name = 2
  # next run
  bb = 3
  ccc = <<-SQL
    multi
    line
    SQL
  d     = 4
  eeeee = 5

  item {}

  f = 6

  g = 7
}
"
    );
}

#[test]
fn blocks_are_separated_by_one_blank_line() {
    let text = render(&[
        WBlock::new("item", vec![])
            .block(WBlock::new("item", vec!["a".into()]))
            .block(WBlock::new("item", vec!["b".into()])),
        WBlock::new("item", vec![]),
    ]);
    assert_eq!(
        text,
        "item {\n  item \"a\" {}\n\n  item \"b\" {}\n}\n\nitem {}\n"
    );
}

#[test]
fn comments() {
    let block = item(vec![
        WItem::Comment("one\ntwo\n".into()),
        attr("a", num("1")),
    ]);
    assert_eq!(
        render(&[block]),
        "item \"x\" {\n  # one\n  # two\n  #\n  a = 1\n}\n"
    );
}

#[test]
fn lists_wrap_at_100_columns() {
    let short: Vec<WExpr> = (0..5).map(|i| s(&format!("value_{i}"))).collect();
    let long: Vec<WExpr> = (0..12).map(|i| s(&format!("value_{i}"))).collect();
    let text = render(&[item(vec![
        attr("short", WExpr::List(short)),
        attr("long", WExpr::List(long)),
        attr("empty", WExpr::List(vec![])),
    ])]);
    assert_eq!(
        text,
        r#"item "x" {
  short = ["value_0", "value_1", "value_2", "value_3", "value_4"]
  long = [
    "value_0",
    "value_1",
    "value_2",
    "value_3",
    "value_4",
    "value_5",
    "value_6",
    "value_7",
    "value_8",
    "value_9",
    "value_10",
    "value_11",
  ]
  empty = []
}
"#
    );
    assert!(text.lines().all(|l| l.chars().count() <= 100));
}

#[test]
fn objects_inline_or_multi_line() {
    let inline = WExpr::Object(vec![("a".into(), num("1")), ("b".into(), s("x"))]);
    assert_eq!(render_expr(&inline), "{ a = 1, b = \"x\" }");
    assert_eq!(render_expr(&WExpr::Object(vec![])), "{}");
    let keys = WExpr::Object(vec![
        ("with space".into(), num("1")),
        ("for".into(), num("2")),
        ("null".into(), num("3")),
        ("x-y".into(), num("4")),
    ]);
    assert_eq!(
        render_expr(&keys),
        "{ \"with space\" = 1, \"for\" = 2, \"null\" = 3, x-y = 4 }"
    );
    let nested = WExpr::Object(vec![
        ("query".into(), s("SELECT 1\n")),
        ("enabled".into(), WExpr::Bool(true)),
        ("n".into(), num("1")),
    ]);
    assert_eq!(
        render(&[item(vec![attr("o", nested)])]),
        "item \"x\" {\n  o = {\n    query = <<-SQL\n      SELECT 1\n      SQL\n    enabled = true\n    n       = 1\n  }\n}\n"
    );
}

#[test]
fn scalars() {
    assert_eq!(render_expr(&WExpr::Null), "null");
    assert_eq!(render_expr(&WExpr::Bool(false)), "false");
    assert_eq!(render_expr(&num("-1.50")), "-1.50");
    assert_eq!(
        render_expr(&num("99999999999999999999")),
        "99999999999999999999.0"
    );
    assert_eq!(render_expr(&id("CASCADE")), "CASCADE");
    assert_eq!(render_expr(&id("double precision")), "\"double precision\"");
    assert_eq!(render_expr(&id("null")), "\"null\"");
    assert_eq!(render_expr(&id("var")), "\"var\"");
    assert_eq!(
        render_expr(&WExpr::Call(
            "Map".into(),
            vec![
                WExpr::Call("LowCardinality".into(), vec![id("String")]),
                id("String")
            ]
        )),
        "Map(LowCardinality(String), String)"
    );
    assert_eq!(
        render_expr(&WExpr::Ref(address(
            "table",
            &["app", "odd.name"],
            &[("column", "2fa")]
        ))),
        "table.app[\"odd.name\"].column[\"2fa\"]"
    );
    assert_eq!(render_expr(&WExpr::Sql("now()".into())), "sql(\"now()\")");
}

#[test]
fn string_escapes() {
    assert_eq!(
        render_expr(&s("a\"b\\c\td\re ${x} %{y} $${z} \u{1}")),
        r#""a\"b\\c\td\re $${x} %%{y} $$${z} \u0001""#
    );
}

#[test]
fn heredoc_forms() {
    let render_attr = |v: &str| render(&[item(vec![attr("v", s(v))])]);
    assert_eq!(
        render_attr("SELECT 1\n  FROM t;\n"),
        "item \"x\" {\n  v = <<-SQL\n    SELECT 1\n      FROM t;\n    SQL\n}\n"
    );
    assert_eq!(
        render_attr("a\n\nb"),
        "item \"x\" {\n  v = chomp(<<-SQL\n    a\n\n    b\n    SQL)\n}\n"
    );
    assert_eq!(
        render_attr(" SELECT a,\n    b\n   FROM t;"),
        "item \"x\" {\n  v = chomp(<<SQL\n SELECT a,\n    b\n   FROM t;\nSQL)\n}\n"
    );
    assert_eq!(
        render_attr("x\nSQL\nSQL1;\nSQL2x\n"),
        "item \"x\" {\n  v = <<-SQL2\n    x\n    SQL\n    SQL1;\n    SQL2x\n    SQL2\n}\n"
    );
    assert_eq!(
        render_attr("${a}\n%{b}\n"),
        "item \"x\" {\n  v = <<-SQL\n    $${a}\n    %%{b}\n    SQL\n}\n"
    );
    assert_eq!(
        render_attr("a\r\nb"),
        "item \"x\" {\n  v = \"a\\r\\nb\"\n}\n"
    );
    assert_eq!(
        render(&[item(vec![attr("v", WExpr::Sql("SELECT\n  1".into()))])]),
        "item \"x\" {\n  v = sql(chomp(<<-SQL\n    SELECT\n      1\n    SQL))\n}\n"
    );
}

#[test]
fn heredoc_round_trips() {
    let cases = [
        "SELECT 1\n",
        "SELECT 1",
        "\nBEGIN\n  RETURN 1;\nEND;\n",
        "\n",
        "\n\n",
        "a\n\n",
        " SELECT a,\n    b\n   FROM t;",
        "  indented\n  both\n",
        "\tTabbed\nline\n",
        "trailing spaces  \n  \nx",
        "SQL\nSQL SECURITY DEFINER\n",
        "$${literal} ${x} %{ if } $\n",
        "é\n→\n",
    ];
    let blocks: Vec<WBlock> = cases
        .iter()
        .map(|c| {
            item(vec![
                attr("plain", s(c)),
                attr("sql", WExpr::Sql((*c).to_owned())),
                attr("listed", WExpr::List(vec![s(c), num("1")])),
                attr("call", WExpr::Call("f".into(), vec![s(c), s(c)])),
                attr("object", WExpr::Object(vec![("k".into(), s(c))])),
            ])
        })
        .collect();
    assert_round_trip(&blocks);
}

#[test]
fn special_values_round_trip() {
    let block = item(vec![
        attr("ident", id("CASCADE")),
        attr("ref", WExpr::Ref(address("role", &["r0"], &[]))),
        attr(
            "nested_ref",
            WExpr::Ref(address("role", &["r0"], &[("member", "m0")])),
        ),
        attr("odd_ref", WExpr::Ref(address("role", &["odd.name"], &[]))),
        attr(
            "qualified",
            WExpr::Ref(address("table", &["app", "users"], &[])),
        ),
        attr(
            "engine",
            WExpr::Call(
                "ReplicatedMergeTree".into(),
                vec![s("/clickhouse/tables/{uuid}/{shard}"), s("{replica}")],
            ),
        ),
        attr(
            "numbers",
            WExpr::List(
                ["0", "-7", "1.5", "1.50", "1e10", "-0.25"]
                    .map(num)
                    .to_vec(),
            ),
        ),
        attr("default", WExpr::Sql("now()".into())),
    ]);
    assert_round_trip(&[block]);
}

/// xorshift64*: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "a", "Z", " ", "  ", "\n", "\n", "\t", "\"", "\\", "$", "{", "}", "%", "${", "%{", "$${",
    "%%{", "SQL", "SQL1", "é", "→", "\u{1}", "\r", "x-y", "  SELECT", "END;", "/*", "#",
];

fn gen_string(rng: &mut Rng) -> String {
    (0..rng.below(9)).map(|_| *rng.pick(PIECES)).collect()
}

fn gen_label(rng: &mut Rng) -> String {
    let label = gen_string(rng);
    if label.is_empty() {
        "l".to_owned()
    } else {
        label
    }
}

fn gen_expr(rng: &mut Rng, depth: usize) -> WExpr {
    let scalar_kinds = 7;
    let kinds = if depth == 0 {
        scalar_kinds
    } else {
        scalar_kinds + 3
    };
    match rng.below(kinds) {
        0 => rng
            .pick(&[WExpr::Null, WExpr::Bool(true), WExpr::Bool(false)])
            .clone(),
        1 => num(rng.pick::<&str>(&[
            "0",
            "42",
            "-7",
            "1.5",
            "1.50",
            "1e10",
            "-0.25",
            "12.375",
            "9223372036854775807",
        ])),
        2 | 3 => s(&gen_string(rng)),
        4 => id(rng.pick::<&str>(&[
            "text", "bigint", "CASCADE", "ROW", "PUBLIC", "String", "x_1", "a-b", "role", "table",
        ])),
        5 => WExpr::Sql(gen_string(rng)),
        6 => WExpr::Ref(
            rng.pick(&[
                address("role", &["r0"], &[]),
                address("role", &["odd.name"], &[]),
                address("role", &["r0"], &[("member", "m0")]),
                address("table", &["app", "users"], &[]),
            ])
            .clone(),
        ),
        7 => WExpr::Call(
            rng.pick(&["varchar", "Nullable", "Map", "ZSTD", "ReplicatedMergeTree"])
                .to_string(),
            (0..rng.below(4))
                .map(|_| gen_expr(rng, depth - 1))
                .collect(),
        ),
        8 => WExpr::List(
            (0..rng.below(5))
                .map(|_| gen_expr(rng, depth - 1))
                .collect(),
        ),
        _ => {
            let keys = [
                "a",
                "b_c",
                "with space",
                "for",
                "null",
                "x-y",
                "1abc",
                "é",
                "${k}",
                "",
            ];
            let mut entries: Vec<(String, WExpr)> = Vec::new();
            for _ in 0..rng.below(5) {
                let key = rng.pick(&keys).to_string();
                if !entries.iter().any(|(k, _)| *k == key) {
                    entries.push((key, gen_expr(rng, depth - 1)));
                }
            }
            WExpr::Object(entries)
        }
    }
}

fn gen_block(rng: &mut Rng, depth: usize) -> WBlock {
    let labels = if rng.below(3) == 0 {
        vec![]
    } else {
        vec![gen_label(rng)]
    };
    let names = [
        "a",
        "bb",
        "ccc",
        "for",
        "type",
        "x-y",
        "name",
        "default",
        "long_attribute_name",
    ];
    let mut used = Vec::new();
    let mut items = Vec::new();
    for _ in 0..rng.below(8) {
        match rng.below(10) {
            0 => items.push(WItem::Blank),
            1 => items.push(WItem::Comment(gen_string(rng).replace('\r', ""))),
            2 | 3 if depth > 0 => items.push(WItem::Block(gen_block(rng, depth - 1))),
            _ => {
                let name = *rng.pick(&names);
                if !used.contains(&name) {
                    used.push(name);
                    items.push(WItem::Attr(name.to_owned(), gen_expr(rng, 3)));
                }
            }
        }
    }
    WBlock {
        kind: "item".to_owned(),
        labels,
        items,
    }
}

#[test]
fn random_trees_round_trip() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..400 {
        let blocks: Vec<WBlock> = (0..1 + rng.below(3))
            .map(|_| gen_block(&mut rng, 2))
            .collect();
        let text = assert_round_trip(&blocks);
        assert_eq!(render(&blocks), text, "rendering is deterministic");
    }
}
