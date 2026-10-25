use super::encode;
use crate::decode::decode;
use crate::model::*;
use crate::settings::ChSettings;
use crate::spec::schema_spec;
use indexmap::IndexMap;
use openmig_core::{EncodeOptions, SplitBy};
use openmig_hcl::{LoadOptions, load_sources};
use pretty_assertions::assert_eq;
use std::collections::{BTreeMap, BTreeSet};

fn round_trip(model: &Model, split: SplitBy) -> (Model, String) {
    let settings = ChSettings::default();
    let files = encode(model, &settings, &EncodeOptions { split });
    let text: String = files.iter().map(|f| f.content.clone()).collect();
    let spec = schema_spec(&settings);
    let doc = load_sources(
        files.into_iter().map(|f| (f.path, f.content)).collect(),
        &LoadOptions {
            spec: &spec,
            vars: IndexMap::new(),
        },
    )
    .unwrap_or_else(|e| panic!("{}\n{text}", e.render(false)));
    let decoded = decode(&doc, &settings)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .model;
    (decoded, text)
}

/// What the decoder cannot reproduce from HCL: derived and server-side state.
fn comparable(mut model: Model) -> Model {
    model.dependencies.clear();
    model.server_version = (0, 0);
    model.external_roles.clear();
    model.users.clear();
    for db in model.databases.values_mut() {
        for mv in db.materialized_views.values_mut() {
            mv.columns.clear();
        }
    }
    for policy in model.row_policies.values_mut() {
        policy.storage.clear();
    }
    for db in model.databases.values_mut() {
        for table in db.tables.values_mut() {
            let Some(targets) = &mut table.time_series else {
                continue;
            };
            for (attr, default) in super::DEFAULT_TIME_SERIES {
                let target = match attr {
                    "data" => &mut targets.data,
                    "tags" => &mut targets.tags,
                    _ => &mut targets.metrics,
                };
                if target.as_deref() == Some(default) {
                    *target = None;
                }
            }
            if *targets == TimeSeriesTargets::default() {
                table.time_series = None;
            }
        }
    }
    model
}

fn column(name: &str, data_type: &str) -> Column {
    Column::new(name, data_type)
}

fn settings(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn rich_model() -> Model {
    let mut m = Model {
        server_version: (26, 5),
        ..Model::default()
    };
    m.databases
        .insert("default".into(), Database::new("default", "Atomic"));
    let mut db = Database::new(
        "analytics",
        "Replicated('/clickhouse/databases/analytics', '{shard}', '{replica}')",
    );
    db.comment = Some("Events \"quoted\" ${not a template}".into());

    let mut logs = Table::new(
        "logs",
        "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)",
    );
    let mut ts = column("ts", "DateTime64(9, 'UTC')");
    ts.codec = vec!["Delta(8)".into(), "ZSTD(1)".into()];
    let mut day = column("day", "Date");
    day.default = Some(ColumnDefault {
        kind: DefaultKind::Materialized,
        expression: "toDate(ts)".into(),
    });
    let mut body = column("body", "String");
    body.default = Some(ColumnDefault {
        kind: DefaultKind::Default,
        expression: "'it\\'s'".into(),
    });
    body.comment = Some("raw\nline".into());
    let mut n = column("n", "UInt64");
    n.default = Some(ColumnDefault {
        kind: DefaultKind::Default,
        expression: "0".into(),
    });
    n.ttl = Some("day + toIntervalDay(1)".into());
    let mut flag = column("flag", "Bool");
    flag.default = Some(ColumnDefault {
        kind: DefaultKind::Default,
        expression: "now() > toDateTime(0)".into(),
    });
    let mut raw = column("raw", "String");
    raw.default = Some(ColumnDefault {
        kind: DefaultKind::Ephemeral,
        expression: "defaultValueOfTypeName('String')".into(),
    });
    let mut stats = column("stats", "UInt64");
    stats.statistics = vec!["tdigest".into()];
    stats.settings = settings(&[("max_compress_block_size", "1048576")]);
    let mut odd = column("Events.Name", "Array(LowCardinality(String))");
    odd.codec = vec!["LZ4HC(9)".into()];
    logs.columns = vec![
        ts,
        day,
        body,
        n,
        flag,
        raw,
        stats,
        odd,
        column("status", "Enum8('new' = 1, 'done' = 2)"),
        column("updated_at", "DateTime"),
        column("select", "UInt8"),
    ];
    logs.order_by = vec![
        "day".into(),
        "toDateTime(ts)".into(),
        "`Events.Name`".into(),
        "`select`".into(),
    ];
    logs.primary_key = vec!["day".into()];
    logs.partition_by = Some("toYYYYMM(day)".into());
    logs.sample_by = Some("cityHash64(body)".into());
    logs.ttl = Some("day + toIntervalDay(30)".into());
    logs.settings = settings(&[
        ("index_granularity", "8192"),
        ("storage_policy", "'encrypted'"),
        ("allow_nullable_key", "true"),
        ("merge_max_block_size", "8192.5"),
    ]);
    logs.indexes.insert(
        "body_tokens".into(),
        Index {
            expression: "body".into(),
            index_type: "tokenbf_v1(32768, 3, 0)".into(),
            granularity: 4,
        },
    );
    logs.indexes.insert(
        "pair".into(),
        Index {
            expression: "(day, n)".into(),
            index_type: "minmax".into(),
            granularity: 1,
        },
    );
    logs.projections.insert(
        "by_day".into(),
        Projection {
            query: "SELECT day, count() GROUP BY day".into(),
            settings: settings(&[("index_granularity", "1024")]),
        },
    );
    logs.constraints.insert(
        "positive".into(),
        Constraint {
            kind: ConstraintKind::Check,
            expression: "n >= 0".into(),
        },
    );
    logs.constraints.insert(
        "known".into(),
        Constraint {
            kind: ConstraintKind::Assume,
            expression: "body != ''".into(),
        },
    );
    logs.comment = Some("Application logs".into());
    db.tables.insert("logs".into(), logs);

    let mut empty_key = Table::new("plain", "MergeTree");
    empty_key.columns = vec![column("x", "UInt8")];
    empty_key.settings = settings(&[("index_granularity", "8192")]);
    db.tables.insert("plain".into(), empty_key);
    let mut memory = Table::new("mem", "Memory");
    memory.columns = vec![column("x", "UInt8")];
    db.tables.insert("mem".into(), memory);

    let mut metrics = Table::new("metrics", "TimeSeries");
    metrics.time_series = Some(TimeSeriesTargets {
        data: Some("ENGINE = MergeTree ORDER BY (id, timestamp)".into()),
        tags: Some(
            "ENGINE = AggregatingMergeTree PRIMARY KEY metric_name ORDER BY tuple(metric_name, id)"
                .into(),
        ),
        metrics: Some("ENGINE = ReplicatedReplacingMergeTree ORDER BY metric_family_name".into()),
    });
    db.tables.insert("metrics".into(), metrics);

    db.views.insert(
        "v".into(),
        View {
            name: "v".into(),
            query: "SELECT day FROM analytics.logs".into(),
            sql_security: Some("INVOKER".into()),
            definer: Some("deployer".into()),
            comment: Some("view".into()),
        },
    );
    db.materialized_views.insert(
        "mv".into(),
        MaterializedView {
            name: "mv".into(),
            to: Some(QualifiedName::new("analytics", "plain")),
            query: "SELECT 1 AS x".into(),
            columns: vec![column("x", "UInt8")],
            refresh: Some("EVERY 1 HOUR".into()),
            comment: Some("mv".into()),
            ..MaterializedView::default()
        },
    );
    db.materialized_views.insert(
        "inner".into(),
        MaterializedView {
            name: "inner".into(),
            query: "SELECT day, count() AS c FROM analytics.logs GROUP BY day".into(),
            inner: Some(InnerTable {
                engine: "SummingMergeTree".into(),
                order_by: vec!["day".into()],
                primary_key: vec!["day".into()],
                partition_by: Some("tuple()".into()),
                settings: settings(&[("index_granularity", "8192")]),
                ..InnerTable::default()
            }),
            ..MaterializedView::default()
        },
    );
    db.dictionaries.insert(
        "d".into(),
        Dictionary {
            name: "d".into(),
            attributes: vec![
                DictionaryAttribute {
                    name: "id".into(),
                    data_type: "UInt64".into(),
                    ..DictionaryAttribute::default()
                },
                DictionaryAttribute {
                    name: "name".into(),
                    data_type: "String".into(),
                    default: Some("'?'".into()),
                    expression: Some("upper(name)".into()),
                    injective: true,
                    ..DictionaryAttribute::default()
                },
            ],
            primary_key: vec!["id".into()],
            source: "CLICKHOUSE(TABLE 'logs' DB 'analytics')".into(),
            lifetime: Some("MIN 0 MAX 300".into()),
            layout: "HASHED()".into(),
            range: None,
            settings: settings(&[("format_csv_allow_single_quotes", "0")]),
            comment: Some("dict".into()),
        },
    );
    m.databases.insert("analytics".into(), db);

    m.functions.insert(
        "linear".into(),
        Function {
            name: "linear".into(),
            parameters: vec!["x".into(), "k".into()],
            body: "(k * x) + 1".into(),
        },
    );
    m.functions.insert(
        "answer".into(),
        Function {
            name: "answer".into(),
            parameters: vec![],
            body: "42".into(),
        },
    );
    for (name, storage) in [("reader", "local_directory"), ("admin", "replicated")] {
        m.roles.insert(
            name.into(),
            Role {
                name: name.into(),
                storage: storage.into(),
            },
        );
    }
    m.role_grants = vec![
        RoleGrant {
            grantee: Grantee::Role("admin".into()),
            role: "reader".into(),
            admin_option: true,
        },
        RoleGrant {
            grantee: Grantee::Role("admin".into()),
            role: "outside".into(),
            admin_option: false,
        },
    ];
    m.grants = vec![
        Grant {
            grantee: Grantee::Role("admin".into()),
            database: Some("analytics".into()),
            privileges: BTreeSet::from(["ALL".to_owned()]),
            grant_option: true,
            ..Grant::default()
        },
        Grant {
            grantee: Grantee::Role("reader".into()),
            database: Some("analytics".into()),
            table: Some("logs".into()),
            columns: vec!["body".into(), "day".into()],
            privileges: BTreeSet::from(["ALTER UPDATE".to_owned(), "SELECT".to_owned()]),
            ..Grant::default()
        },
        Grant {
            grantee: Grantee::Role("reader".into()),
            privileges: BTreeSet::from(["dictGet".to_owned(), "SHOW".to_owned()]),
            ..Grant::default()
        },
        Grant {
            grantee: Grantee::Role("reader".into()),
            database: Some("elsewhere".into()),
            privileges: BTreeSet::from(["SELECT".to_owned()]),
            ..Grant::default()
        },
    ];
    m.row_policies.insert(
        RowPolicyId {
            database: "analytics".into(),
            table: "logs".into(),
            name: "tenant".into(),
        },
        RowPolicy {
            filter: Some("body != 'x'".into()),
            restrictive: true,
            to: RoleSet {
                all: true,
                names: vec![],
                except: vec!["admin".into()],
            },
            storage: "local_directory".into(),
        },
    );
    m.normalize();
    m
}

#[test]
fn rich_model_round_trips() {
    let model = rich_model();
    for split in [SplitBy::None, SplitBy::Schema, SplitBy::Kind] {
        let (decoded, text) = round_trip(&model, split);
        assert_eq!(comparable(decoded), comparable(model.clone()), "\n{text}");
    }
}

#[test]
fn short_forms() {
    let (_, text) = round_trip(&rich_model(), SplitBy::None);
    let mut squeezed = text.clone();
    while squeezed.contains("  ") {
        squeezed = squeezed.replace("  ", " ");
    }
    for expected in [
        r#"engine = Replicated("/clickhouse/databases/analytics", "{shard}", "{replica}")"#,
        "type = DateTime64(9, \"UTC\")",
        "codec = [Delta(8), ZSTD(1)]",
        r#"type = "Enum8('new' = 1, 'done' = 2)""#,
        "order_by = [column.day, \"toDateTime(ts)\", column[\"Events.Name\"], \"`select`\"]",
        "default = \"it's\"",
        "default = 0",
        "storage = local_directory",
        "layout = HASHED()",
        "on = [database.analytics]",
        "with_grant_option = true",
        "privileges = [\"ALTER UPDATE\", SELECT]",
        "member_of = [\"outside\", { role = role.reader, admin = true }]",
    ] {
        assert!(
            squeezed.contains(expected),
            "missing `{expected}` in\n{text}"
        );
    }
    assert!(!text.contains("database \"default\""), "{text}");
    assert!(!text.contains("tags"), "{text}");
    assert!(
        text.contains("metrics = \"ENGINE = ReplicatedReplacingMergeTree"),
        "{text}"
    );
}

#[test]
fn empty_model() {
    let files = encode(
        &Model::default(),
        &ChSettings::default(),
        &EncodeOptions::default(),
    );
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content, "");
}
