use super::decode;
use crate::model::*;
use crate::settings::ChSettings;
use crate::spec::schema_spec;
use indexmap::IndexMap;
use openmig_core::{Error, ObjectKind, ObjectRef, RenameHint};
use openmig_hcl::{LoadOptions, load_sources};
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn try_decode(src: &str) -> Result<openmig_core::Desired<Model>, String> {
    let settings = ChSettings::default();
    let spec = schema_spec(&settings);
    let opts = LoadOptions {
        spec: &spec,
        vars: IndexMap::new(),
    };
    let doc = load_sources(vec![(PathBuf::from("schema.hcl"), src.to_owned())], &opts)
        .map_err(|e| e.render(false))?;
    decode(&doc, &settings).map_err(|e| match e {
        Error::Schema { rendered } => rendered,
        other => other.to_string(),
    })
}

fn decode_ok(src: &str) -> Model {
    try_decode(src)
        .unwrap_or_else(|e| panic!("decode failed:\n{e}"))
        .model
}

fn decode_err(src: &str) -> String {
    match try_decode(src) {
        Ok(_) => panic!("decoding succeeded"),
        Err(e) => e,
    }
}

fn table<'m>(model: &'m Model, database: &str, name: &str) -> &'m Table {
    model
        .table(&QualifiedName::new(database, name))
        .unwrap_or_else(|| panic!("no table {database}.{name}"))
}

#[test]
fn databases_and_defaults() {
    let model = decode_ok(
        r#"
        database "dialo" {
          engine  = Replicated("/clickhouse/databases/dialo", "{shard}", "{replica}")
          comment = "main"
        }
        database "plain" {}
        "#,
    );
    assert_eq!(
        model.databases.keys().collect::<Vec<_>>(),
        ["default", "dialo", "plain"]
    );
    let dialo = &model.databases["dialo"];
    assert_eq!(
        dialo.engine,
        "Replicated('/clickhouse/databases/dialo', '{shard}', '{replica}')"
    );
    assert_eq!(dialo.comment.as_deref(), Some("main"));
    assert_eq!(model.databases["plain"].engine, "Atomic");
    assert_eq!(model.databases["default"].engine, "Atomic");
}

#[test]
fn tables_and_columns() {
    let model = decode_ok(
        r#"
        database "analytics" {}

        table "analytics.logs" {
          engine = ReplicatedReplacingMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}", updated_at)
          column "service" {
            type  = LowCardinality(String)
            codec = [ZSTD(1)]
          }
          column "ts" {
            type  = DateTime64(9, "UTC")
            codec = [Delta(8), ZSTD(1)]
          }
          column "day" {
            type         = Date
            materialized = "toDate(ts)"
          }
          column "body" {
            type    = String
            default = "it's"
            comment = "raw"
          }
          column "n" {
            type    = UInt64
            default = 0
            ttl     = "day + toIntervalDay(1)"
          }
          column "status" {
            type = "Enum8('new' = 1, 'done' = 2)"
          }
          column "attrs" {
            type  = Map(LowCardinality(String), String)
            alias = "mapKeys(attrs)"
          }
          column "raw" {
            type      = String
            ephemeral = ""
          }
          column "Events.Name" {
            type  = Array(String)
            codec = "CODEC(ZSTD(1))"
          }
          column "updated_at" {
            type    = DateTime
            default = sql("now()")
          }
          order_by     = [column.service, column.day, "toDateTime(ts)", column["Events.Name"]]
          primary_key  = [column.service]
          partition_by = "toYYYYMM(day)"
          sample_by    = "cityHash64(body)"
          ttl          = "day + toIntervalDay(30)"
          settings     = { index_granularity = 4096, storage_policy = "encrypted", allow_nullable_key = true }
          comment      = "Application logs"

          index "body_tokens" {
            expr        = "body"
            type        = tokenbf_v1(32768, 3, 0)
            granularity = 4
          }
          index "minmax_n" {
            expr = "n"
            type = minmax
          }
          projection "by_service" {
            query = "SELECT service, count() GROUP BY service"
          }
          constraint "positive" {
            check = "n >= 0"
          }
          constraint "known" {
            assume = "service != ''"
          }
        }
        "#,
    );
    let t = table(&model, "analytics", "logs");
    assert_eq!(
        t.engine,
        "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)"
    );
    let types: Vec<&str> = t.columns.iter().map(|c| c.data_type.as_str()).collect();
    assert_eq!(
        types,
        [
            "LowCardinality(String)",
            "DateTime64(9, 'UTC')",
            "Date",
            "String",
            "UInt64",
            "Enum8('new' = 1, 'done' = 2)",
            "Map(LowCardinality(String), String)",
            "String",
            "Array(String)",
            "DateTime",
        ]
    );
    let default = |i: usize| t.columns[i].default.clone().map(|d| (d.kind, d.expression));
    assert_eq!(
        default(2),
        Some((DefaultKind::Materialized, "toDate(ts)".into()))
    );
    assert_eq!(default(3), Some((DefaultKind::Default, "'it\\'s'".into())));
    assert_eq!(default(4), Some((DefaultKind::Default, "0".into())));
    assert_eq!(
        default(6),
        Some((DefaultKind::Alias, "mapKeys(attrs)".into()))
    );
    assert_eq!(default(7), Some((DefaultKind::Ephemeral, String::new())));
    assert_eq!(default(9), Some((DefaultKind::Default, "now()".into())));
    assert_eq!(t.columns[1].codec, ["Delta(8)", "ZSTD(1)"]);
    assert_eq!(t.columns[8].codec, ["ZSTD(1)"]);
    assert_eq!(t.columns[3].comment.as_deref(), Some("raw"));
    assert_eq!(t.columns[4].ttl.as_deref(), Some("day + toIntervalDay(1)"));
    assert_eq!(
        t.order_by,
        ["service", "day", "toDateTime(ts)", "`Events.Name`"]
    );
    assert_eq!(t.primary_key, ["service"]);
    assert_eq!(t.partition_by.as_deref(), Some("toYYYYMM(day)"));
    assert_eq!(t.sample_by.as_deref(), Some("cityHash64(body)"));
    assert_eq!(t.ttl.as_deref(), Some("day + toIntervalDay(30)"));
    assert_eq!(t.settings["index_granularity"], "4096");
    assert_eq!(t.settings["storage_policy"], "'encrypted'");
    assert_eq!(t.settings["allow_nullable_key"], "true");
    assert_eq!(
        t.indexes["body_tokens"].index_type,
        "tokenbf_v1(32768, 3, 0)"
    );
    assert_eq!(t.indexes["body_tokens"].granularity, 4);
    assert_eq!(t.indexes["minmax_n"].granularity, 1);
    assert_eq!(
        t.projections["by_service"].query,
        "SELECT service, count() GROUP BY service"
    );
    assert_eq!(t.constraints["positive"].kind, ConstraintKind::Check);
    assert_eq!(t.constraints["known"].kind, ConstraintKind::Assume);
    assert_eq!(t.comment.as_deref(), Some("Application logs"));
}

#[test]
fn keys_default_to_order_by() {
    let model = decode_ok(
        r#"
        table "t" {
          engine = MergeTree
          column "a" {
            type = UInt8
          }
          order_by = "tuple()"
        }
        table "u" {
          engine = MergeTree
          column "a" {
            type = UInt8
          }
          order_by = "(a, a + 1)"
        }
        "#,
    );
    let t = table(&model, "default", "t");
    assert!(t.order_by.is_empty() && t.primary_key.is_empty());
    let u = table(&model, "default", "u");
    assert_eq!(u.order_by, ["a", "a + 1"]);
    assert_eq!(u.primary_key, u.order_by);
}

#[test]
fn time_series_tables_have_no_columns() {
    let model = decode_ok(
        r#"
        database "otel" {}
        table "otel.metrics" {
          engine = TimeSeries
        }
        "#,
    );
    let t = table(&model, "otel", "metrics");
    assert_eq!(t.engine, "TimeSeries");
    assert!(t.columns.is_empty() && t.time_series.is_none());
    let err = decode_err(
        r#"
        table "metrics" {
          engine = TimeSeries
          column "x" {
            type = String
          }
        }
        "#,
    );
    assert!(err.contains("TimeSeries"), "{err}");
}

#[test]
fn views_dictionaries_functions() {
    let model = decode_ok(
        r#"
        database "app" {}
        table "app.src" {
          engine = MergeTree
          column "x" {
            type = UInt32
          }
          order_by = [column.x]
        }
        table "app.dst" {
          engine = MergeTree
          column "x" {
            type = UInt32
          }
          order_by = [column.x]
        }
        materialized_view "app.copy" {
          to      = table.app.dst
          as      = "SELECT x FROM app.src"
          comment = "copy"
        }
        materialized_view "app.inner" {
          engine   = SummingMergeTree
          order_by = ["x"]
          as       = "SELECT x FROM app.src"
          refresh  = "EVERY 1 HOUR"
        }
        materialized_view "app.elsewhere" {
          to = "other.t"
          as = "SELECT 1"
        }
        view "app.v" {
          as           = "SELECT x FROM app.src"
          sql_security = invoker
          definer      = "deployer"
        }
        dictionary "app.d" {
          attribute "x" {
            type = UInt32
          }
          attribute "name" {
            type         = String
            default      = "?"
            hierarchical = false
            injective    = true
          }
          primary_key = [attribute.x]
          source      = "CLICKHOUSE(TABLE 'src' DB 'app')"
          layout      = HASHED
          lifetime    = 300
        }
        function "linear" {
          args = ["x", "k"]
          body = "k * x"
        }
        "#,
    );
    let app = &model.databases["app"];
    let copy = &app.materialized_views["copy"];
    assert_eq!(copy.to, Some(QualifiedName::new("app", "dst")));
    assert_eq!(copy.comment.as_deref(), Some("copy"));
    let inner = app.materialized_views["inner"].inner.as_ref().unwrap();
    assert_eq!(inner.engine, "SummingMergeTree");
    assert_eq!(inner.order_by, ["x"]);
    assert_eq!(
        app.materialized_views["inner"].refresh.as_deref(),
        Some("EVERY 1 HOUR")
    );
    assert_eq!(
        app.materialized_views["elsewhere"].to,
        Some(QualifiedName::new("other", "t"))
    );
    let v = &app.views["v"];
    assert_eq!(v.sql_security.as_deref(), Some("INVOKER"));
    assert_eq!(v.definer.as_deref(), Some("deployer"));
    let d = &app.dictionaries["d"];
    assert_eq!(d.primary_key, ["x"]);
    assert_eq!(d.layout, "HASHED()");
    assert_eq!(d.lifetime.as_deref(), Some("300"));
    assert_eq!(d.attributes[1].default.as_deref(), Some("'?'"));
    assert!(d.attributes[1].injective);
    assert_eq!(model.functions["linear"].parameters, ["x", "k"]);
    assert_eq!(model.functions["linear"].body, "k * x");
    let copy_ref = ObjectRef::new(ObjectKind::MaterializedView, ["app", "copy"]);
    assert!(model.dependencies.iter().any(|d| d.from == copy_ref
        && d.to == ObjectRef::new(ObjectKind::Table, ["app", "dst"])
        && !d.soft));
    assert!(model.dependencies.iter().any(|d| d.from == copy_ref
        && d.to == ObjectRef::new(ObjectKind::Table, ["app", "src"])
        && d.soft));
}

#[test]
fn roles_grants_and_row_policies() {
    let model = decode_ok(
        r#"
        database "dialo" {}
        table "dialo.events" {
          engine = MergeTree
          column "call_id" {
            type = String
          }
          order_by = [column.call_id]
        }
        role "reader" {
          storage = local_directory
        }
        role "admin" {
          storage   = "replicated"
          member_of = [role.reader, { role = "outside", admin = true }]
        }
        grant {
          privileges = [select, ALTER_UPDATE, dictGet, "show tables"]
          on         = [database.dialo, database.default, "*.*", "other.t"]
          to         = [role.reader, "external"]
        }
        grant {
          privileges = [ALL, SELECT]
          on         = [table.dialo.events]
          columns    = [table.dialo.events.column.call_id]
          to         = [role.admin]
          with_grant_option = true
        }
        row_policy "tenant" {
          on     = table.dialo.events
          using  = "call_id != ''"
          as     = RESTRICTIVE
          to     = [ALL]
          except = [role.admin]
        }
        "#,
    );
    assert_eq!(model.roles["admin"].storage, "replicated");
    assert_eq!(model.roles["reader"].storage, "local_directory");
    assert_eq!(
        model.role_grants,
        vec![
            RoleGrant {
                grantee: Grantee::Role("admin".into()),
                role: "outside".into(),
                admin_option: true,
            },
            RoleGrant {
                grantee: Grantee::Role("admin".into()),
                role: "reader".into(),
                admin_option: false,
            },
        ]
    );
    let targets: Vec<String> = model
        .grants
        .iter()
        .map(|g| format!("{} {}", g.grantee.name(), g.target_sql()))
        .collect();
    assert_eq!(
        targets,
        [
            "admin `dialo`.`events`",
            "external *.*",
            "external `default`.*",
            "external `dialo`.*",
            "external `other`.`t`",
            "reader *.*",
            "reader `default`.*",
            "reader `dialo`.*",
            "reader `other`.`t`",
        ]
    );
    let reader = &model.grants[7];
    assert_eq!(
        reader.privileges,
        BTreeSet::from([
            "ALTER UPDATE".to_owned(),
            "SELECT".to_owned(),
            "SHOW TABLES".to_owned(),
            "dictGet".to_owned(),
        ])
    );
    let admin = &model.grants[0];
    assert_eq!(admin.privileges, BTreeSet::from(["ALL".to_owned()]));
    assert_eq!(admin.columns, ["call_id"]);
    assert!(admin.grant_option);
    assert_eq!(
        model.external_roles,
        BTreeSet::from(["external".to_owned(), "outside".to_owned()])
    );
    let policy = &model.row_policies[&RowPolicyId {
        database: "dialo".into(),
        table: "events".into(),
        name: "tenant".into(),
    }];
    assert!(policy.restrictive);
    assert!(policy.to.all);
    assert_eq!(policy.to.except, ["admin"]);
    assert_eq!(policy.filter.as_deref(), Some("call_id != ''"));
}

#[test]
fn hints_dependencies_and_origins() {
    let desired = try_decode(
        r#"
        database "app" {}
        table "app.visits" {
          renamed_from = "app.hits"
          engine = MergeTree
          column "page" {
            type         = String
            renamed_from = "url"
          }
          order_by = [column.page]
        }
        view "app.v" {
          as         = "SELECT 1"
          depends_on = [table.app.visits]
        }
        "#,
    )
    .unwrap();
    assert_eq!(
        desired.hints.0,
        vec![
            RenameHint {
                kind: ObjectKind::Column,
                from: vec!["app".into(), "visits".into(), "url".into()],
                to: vec!["app".into(), "visits".into(), "page".into()],
            },
            RenameHint {
                kind: ObjectKind::Table,
                from: vec!["app".into(), "hits".into()],
                to: vec!["app".into(), "visits".into()],
            },
        ]
    );
    let view = ObjectRef::new(ObjectKind::View, ["app", "v"]);
    assert!(desired.model.dependencies.iter().any(|d| d.from == view
        && d.to == ObjectRef::new(ObjectKind::Table, ["app", "visits"])
        && !d.soft));
    for object in [
        ObjectRef::new(ObjectKind::Database, ["app"]),
        ObjectRef::new(ObjectKind::Table, ["app", "visits"]),
        ObjectRef::new(ObjectKind::Column, ["app", "visits", "page"]),
        view,
    ] {
        assert!(desired.origins.contains_key(&object), "{object}");
    }
}

#[test]
fn errors() {
    let cases = [
        (
            r#"table "t" {
                 column "a" { type = UInt8 }
               }"#,
            "requires the attribute `engine`",
        ),
        (
            r#"table "t" {
                 engine = MergeTree
                 column "a" {
                   type    = UInt8
                   default = 1
                   alias   = "2"
                 }
               }"#,
            "at most one of",
        ),
        (
            r#"table "t" {
                 engine = MergeTree
                 column "a" { type = UInt8 }
                 order_by = ["a"]
                 enigne = MergeTree
               }"#,
            "did you mean `engine`",
        ),
        (
            r#"table "t" {
                 engine = MergeTree
                 column "a" { type = UInt8 }
               }
               table "u" {
                 engine   = MergeTree
                 column "b" { type = UInt8 }
                 order_by = [table.default.t.column.a]
               }"#,
            "column of another table",
        ),
        (
            r#"table "t" {
                 engine = MergeTree
               }
               view "t" {
                 as = "SELECT 1"
               }"#,
            "share one namespace",
        ),
        (
            r#"data "t" {
                 rows = []
               }"#,
            "not supported for ClickHouse",
        ),
        (
            r#"grant {
                 privileges = [SELECT]
                 on         = [database.default]
                 columns    = ["a"]
                 to         = ["r"]
               }"#,
            "`columns` can only be granted",
        ),
        (
            r#"materialized_view "mv" {
                 as = "SELECT 1"
               }"#,
            "needs `to`",
        ),
        (
            r#"table "system.t" {
                 engine = Memory
               }"#,
            "system database",
        ),
    ];
    for (src, expected) in cases {
        let err = decode_err(src);
        assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
    }
}
