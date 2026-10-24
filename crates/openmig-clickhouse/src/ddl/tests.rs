use super::*;
use pretty_assertions::assert_eq;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn map(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn replicated_table_with_codecs_indexes_ttl_and_settings() {
    let ddl = "CREATE TABLE logs.app_logs (`Timestamp` DateTime64(9) CODEC(Delta(8), ZSTD(1)), \
        `TimestampTime` DateTime DEFAULT toDateTime(Timestamp), `ServiceName` LowCardinality(String) CODEC(ZSTD(1)), \
        `Attrs` Map(LowCardinality(String), String) CODEC(ZSTD(1)), `Body` String CODEC(ZSTD(1)), \
        INDEX idx_body Body TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 8, \
        INDEX idx_attr_key mapKeys(Attrs) TYPE bloom_filter(0.01) GRANULARITY 1) \
        ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}') \
        PARTITION BY toDate(TimestampTime) PRIMARY KEY (ServiceName, TimestampTime) \
        ORDER BY (ServiceName, TimestampTime, Timestamp) TTL TimestampTime + toIntervalDay(180) \
        SETTINGS index_granularity = 8192, storage_policy = 'encrypted', ttl_only_drop_parts = 1";
    let table = parse_table(ddl).unwrap();
    assert_eq!(
        table.name,
        ParsedName {
            database: Some("logs".into()),
            name: "app_logs".into()
        }
    );
    assert_eq!(
        table.storage,
        StorageClauses {
            engine: "ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')".into(),
            partition_by: Some("toDate(TimestampTime)".into()),
            primary_key: Some(strings(&["ServiceName", "TimestampTime"])),
            order_by: Some(strings(&["ServiceName", "TimestampTime", "Timestamp"])),
            sample_by: None,
            ttl: Some("TimestampTime + toIntervalDay(180)".into()),
            settings: map(&[
                ("index_granularity", "8192"),
                ("storage_policy", "'encrypted'"),
                ("ttl_only_drop_parts", "1"),
            ]),
            targets: BTreeMap::new(),
        }
    );
    let columns = &table.elements.columns;
    assert_eq!(columns.len(), 5);
    assert_eq!(columns[0].name, "Timestamp");
    assert_eq!(columns[0].data_type, "DateTime64(9)");
    assert_eq!(columns[0].codec, strings(&["Delta(8)", "ZSTD(1)"]));
    assert_eq!(
        columns[1].default,
        Some(("DEFAULT".into(), "toDateTime(Timestamp)".into()))
    );
    assert_eq!(columns[3].data_type, "Map(LowCardinality(String), String)");
    assert_eq!(
        table.elements.indexes,
        vec![
            ParsedIndex {
                name: "idx_body".into(),
                expression: "Body".into(),
                index_type: "tokenbf_v1(32768, 3, 0)".into(),
                granularity: Some(8),
            },
            ParsedIndex {
                name: "idx_attr_key".into(),
                expression: "mapKeys(Attrs)".into(),
                index_type: "bloom_filter(0.01)".into(),
                granularity: Some(1),
            },
        ]
    );
    assert_eq!(table.comment, None);
}

#[test]
fn replacing_merge_tree_with_everything() {
    let ddl = "CREATE TABLE app.calls (`id` UInt64, `tenant` LowCardinality(String), \
        `status` Enum8('new' = 1, 'done' = 2), `pairs` Array(Tuple(DateTime64(3), Float64)), \
        `note` Nullable(String) TTL created + toIntervalDay(1), \
        `created` DateTime DEFAULT now() CODEC(Delta(4), ZSTD(1)), \
        `created_date` Date MATERIALIZED toDate(created), `tenant_upper` String ALIAS upper(tenant), \
        `raw` String EPHEMERAL, `z` LowCardinality(Nullable(String)) DEFAULT NULL COMMENT 'it\\'s z' CODEC(LZ4HC(9)) TTL created + toIntervalDay(2), \
        `updated_at` DateTime64(6, 'UTC'), \
        INDEX idx_mm id TYPE minmax GRANULARITY 1, \
        CONSTRAINT id_positive CHECK id > 0, CONSTRAINT tenant_known ASSUME tenant != '', \
        PROJECTION by_tenant (SELECT tenant, count() GROUP BY tenant), \
        PROJECTION p_sorted (SELECT * ORDER BY tenant) WITH SETTINGS (index_granularity = 4096)) \
        ENGINE = ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at) \
        PARTITION BY toYYYYMM(created) PRIMARY KEY (tenant, id) ORDER BY (tenant, id, created) SAMPLE BY id \
        TTL created + toIntervalDay(30), created + toIntervalDay(7) TO DISK 'cold' \
        SETTINGS storage_policy = 'encrypted', index_granularity = 8192 COMMENT 'calls\\ntable'";
    let table = parse_table(ddl).unwrap();
    let s = &table.storage;
    assert_eq!(
        s.engine,
        "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', updated_at)"
    );
    assert_eq!(s.sample_by.as_deref(), Some("id"));
    assert_eq!(
        s.ttl.as_deref(),
        Some("created + toIntervalDay(30), created + toIntervalDay(7) TO DISK 'cold'")
    );
    assert_eq!(table.comment.as_deref(), Some("calls\ntable"));

    let c = &table.elements.columns;
    assert_eq!(c[2].data_type, "Enum8('new' = 1, 'done' = 2)");
    assert_eq!(c[4].ttl.as_deref(), Some("created + toIntervalDay(1)"));
    assert_eq!(c[4].data_type, "Nullable(String)");
    assert_eq!(
        c[6].default,
        Some(("MATERIALIZED".into(), "toDate(created)".into()))
    );
    assert_eq!(c[7].default, Some(("ALIAS".into(), "upper(tenant)".into())));
    assert_eq!(c[8].default, Some(("EPHEMERAL".into(), String::new())));
    assert_eq!(c[9].default, Some(("DEFAULT".into(), "NULL".into())));
    assert_eq!(c[9].comment.as_deref(), Some("it's z"));
    assert_eq!(c[9].codec, strings(&["LZ4HC(9)"]));
    assert_eq!(c[9].ttl.as_deref(), Some("created + toIntervalDay(2)"));
    assert_eq!(c[10].data_type, "DateTime64(6, 'UTC')");

    assert_eq!(
        table.elements.constraints,
        vec![
            ParsedConstraint {
                name: "id_positive".into(),
                kind: "CHECK".into(),
                expression: "id > 0".into(),
            },
            ParsedConstraint {
                name: "tenant_known".into(),
                kind: "ASSUME".into(),
                expression: "tenant != ''".into(),
            },
        ]
    );
    assert_eq!(
        table.elements.projections,
        vec![
            ParsedProjection {
                name: "by_tenant".into(),
                query: "SELECT tenant, count() GROUP BY tenant".into(),
                settings: BTreeMap::new(),
            },
            ParsedProjection {
                name: "p_sorted".into(),
                query: "SELECT * ORDER BY tenant".into(),
                settings: map(&[("index_granularity", "4096")]),
            },
        ]
    );
}

#[test]
fn keys() {
    assert_eq!(split_key("(a, b)").unwrap(), strings(&["a", "b"]));
    assert_eq!(
        split_key("tuple(a, b DESC)").unwrap(),
        strings(&["a", "b DESC"])
    );
    assert_eq!(split_key("tuple()").unwrap(), Vec::<String>::new());
    assert_eq!(split_key("timestamp").unwrap(), strings(&["timestamp"]));
    assert_eq!(split_key("(id)").unwrap(), strings(&["id"]));
    assert_eq!(
        split_key("(ServiceName, SpanName, toDateTime(Timestamp))").unwrap(),
        strings(&["ServiceName", "SpanName", "toDateTime(Timestamp)"])
    );
    assert_eq!(split_key("(a + 1) * 2").unwrap(), strings(&["(a + 1) * 2"]));
    assert_eq!(
        split_key("cityHash64(b)").unwrap(),
        strings(&["cityHash64(b)"])
    );
    assert_eq!(
        split_list("Delta(8), ZSTD(1)").unwrap(),
        strings(&["Delta(8)", "ZSTD(1)"])
    );
}

#[test]
fn table_variants() {
    let plain = parse_table(
        "CREATE TABLE d.local_t (`a` UInt32, `b` String) ENGINE = MergeTree ORDER BY tuple() SETTINGS index_granularity = 8192",
    )
    .unwrap();
    assert_eq!(plain.storage.engine, "MergeTree");
    assert_eq!(plain.storage.order_by, Some(vec![]));
    assert_eq!(plain.storage.primary_key, None);

    let memory = parse_table("CREATE TABLE d.mem (`a` UInt32) ENGINE = Memory").unwrap();
    assert_eq!(memory.storage.engine, "Memory");
    assert_eq!(memory.storage.order_by, None);

    let keeper = parse_table(
        "CREATE TABLE d.km (`k` String, `v` UInt32) ENGINE = KeeperMap('/x') PRIMARY KEY k",
    )
    .unwrap();
    assert_eq!(keeper.storage.primary_key, Some(strings(&["k"])));

    let dist = parse_table(
        "CREATE TABLE d.dist (`a` UInt32, `b` String) ENGINE = Distributed('{cluster}', 'd', 'local_t', rand())",
    )
    .unwrap();
    assert_eq!(
        dist.storage.engine,
        "Distributed('{cluster}', 'd', 'local_t', rand())"
    );

    let keywords = parse_table(
        "CREATE TABLE d.kw (`ttl` UInt32, `select` UInt32, `comment` String, \
         `codec` UInt8 DEFAULT ttl + 1 TTL toDateTime(ttl) + toIntervalDay(1), \
         `settings` UInt8 ALIAS codec + `select`, `d` DateTime, INDEX idx type TYPE set(0) GRANULARITY 1) \
         ENGINE = MergeTree PARTITION BY ttl ORDER BY (`select`, ttl, comment) TTL d + toIntervalDay(1) \
         SETTINGS index_granularity = 1024 COMMENT 'c'",
    )
    .unwrap();
    let c = &keywords.elements.columns;
    assert_eq!(c[0].name, "ttl");
    assert_eq!(c[3].default, Some(("DEFAULT".into(), "ttl + 1".into())));
    assert_eq!(
        c[3].ttl.as_deref(),
        Some("toDateTime(ttl) + toIntervalDay(1)")
    );
    assert_eq!(
        c[4].default,
        Some(("ALIAS".into(), "codec + `select`".into()))
    );
    assert_eq!(keywords.storage.partition_by.as_deref(), Some("ttl"));
    assert_eq!(
        keywords.storage.order_by,
        Some(strings(&["`select`", "ttl", "comment"]))
    );
    assert_eq!(keywords.elements.indexes[0].expression, "type");
    assert_eq!(keywords.elements.indexes[0].index_type, "set(0)");
    assert_eq!(keywords.comment.as_deref(), Some("c"));

    let inline_pk = parse_table(
        "CREATE TABLE d.inl (`id` UInt64, `s` String DEFAULT 'a TTL, b SETTINGS x' COMMENT 'TTL ORDER BY') \
         ENGINE = MergeTree PRIMARY KEY tuple(id) ORDER BY (id) SETTINGS storage_policy = 'encrypted', merge_with_ttl_timeout = 3600",
    )
    .unwrap();
    assert_eq!(inline_pk.storage.primary_key, Some(strings(&["id"])));
    assert_eq!(
        inline_pk.elements.columns[1].default,
        Some(("DEFAULT".into(), "'a TTL, b SETTINGS x'".into()))
    );

    let partitioned = parse_table(
        "CREATE TABLE d.pt (`d` Date, `t` String, `s` UInt64 STATISTICS(tdigest) SETTINGS (max_compress_block_size = 1048576), \
         `x.y` Nullable(Int32)) ENGINE = MergeTree PARTITION BY (toYYYYMM(d), t) ORDER BY (t, d)",
    )
    .unwrap();
    assert_eq!(
        partitioned.storage.partition_by.as_deref(),
        Some("(toYYYYMM(d), t)")
    );
    let s = &partitioned.elements.columns[2];
    assert_eq!(s.statistics, strings(&["tdigest"]));
    assert_eq!(s.settings, map(&[("max_compress_block_size", "1048576")]));
    assert_eq!(partitioned.elements.columns[3].name, "x.y");

    let group_by_ttl = parse_table(
        "CREATE TABLE d.pt2 (`d` Date, `weird name` String) ENGINE = ReplacingMergeTree ORDER BY d \
         TTL d + toIntervalMonth(1) GROUP BY d SET `weird name` = any(`weird name`) SETTINGS index_granularity = 8192",
    )
    .unwrap();
    assert_eq!(group_by_ttl.elements.columns[1].name, "weird name");
    assert_eq!(
        group_by_ttl.storage.ttl.as_deref(),
        Some("d + toIntervalMonth(1) GROUP BY d SET `weird name` = any(`weird name`)")
    );
}

#[test]
fn time_series() {
    let bare = parse_table(
        "CREATE TABLE otel.metrics (`id` UUID DEFAULT reinterpretAsUUID(sipHash128(metric_name, all_tags)), \
         `timestamp` DateTime64(3), `value` Float64) ENGINE = TimeSeries \
         DATA ENGINE = MergeTree ORDER BY (id, timestamp) \
         TAGS ENGINE = AggregatingMergeTree PRIMARY KEY metric_name ORDER BY tuple(metric_name, id) \
         METRICS ENGINE = ReplacingMergeTree ORDER BY metric_family_name",
    )
    .unwrap();
    assert_eq!(bare.storage.engine, "TimeSeries");
    assert_eq!(bare.storage.order_by, None);
    assert_eq!(
        bare.storage.targets,
        map(&[
            ("DATA", "ENGINE = MergeTree ORDER BY (id, timestamp)"),
            (
                "METRICS",
                "ENGINE = ReplacingMergeTree ORDER BY metric_family_name"
            ),
            (
                "TAGS",
                "ENGINE = AggregatingMergeTree PRIMARY KEY metric_name ORDER BY tuple(metric_name, id)"
            ),
        ])
    );

    let with_settings = parse_table(
        "CREATE TABLE d.tsx (`id` UUID) ENGINE = TimeSeries SETTINGS store_min_time_and_max_time = 0 \
         DATA ENGINE = ReplicatedMergeTree ORDER BY (id, timestamp) TAGS ENGINE = AggregatingMergeTree ORDER BY id \
         METRICS ENGINE = ReplacingMergeTree ORDER BY metric_family_name",
    )
    .unwrap();
    assert_eq!(
        with_settings.storage.settings,
        map(&[("store_min_time_and_max_time", "0")])
    );
    assert_eq!(
        with_settings.storage.targets["DATA"],
        "ENGINE = ReplicatedMergeTree ORDER BY (id, timestamp)"
    );
}

#[test]
fn materialized_views() {
    let to = parse_view(
        "CREATE MATERIALIZED VIEW otel.trace_ids_mv TO otel.trace_ids (`TraceId` String, `Start` DateTime64(9), `End` DateTime64(9)) \
         AS SELECT TraceId, min(Timestamp) AS Start, max(Timestamp) AS End FROM otel.traces WHERE TraceId != '' GROUP BY TraceId",
    )
    .unwrap();
    assert!(to.materialized);
    assert_eq!(
        to.to,
        Some(ParsedName {
            database: Some("otel".into()),
            name: "trace_ids".into()
        })
    );
    assert_eq!(
        to.columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("TraceId", "String"),
            ("Start", "DateTime64(9)"),
            ("End", "DateTime64(9)")
        ]
    );
    assert_eq!(
        to.query,
        "SELECT TraceId, min(Timestamp) AS Start, max(Timestamp) AS End FROM otel.traces WHERE TraceId != '' GROUP BY TraceId"
    );
    assert_eq!(to.storage, None);

    let inner = parse_view(
        "CREATE MATERIALIZED VIEW d.mv_full (`k` String, `d` Date, `c` UInt64) ENGINE = AggregatingMergeTree \
         PARTITION BY toYYYYMM(d) ORDER BY (k, d) TTL d + toIntervalYear(1) SETTINGS index_granularity = 1024 \
         AS SELECT k, d, count() AS c FROM d.src GROUP BY k, d",
    )
    .unwrap();
    let storage = inner.storage.unwrap();
    assert_eq!(storage.engine, "AggregatingMergeTree");
    assert_eq!(storage.order_by, Some(strings(&["k", "d"])));
    assert_eq!(storage.ttl.as_deref(), Some("d + toIntervalYear(1)"));
    assert_eq!(storage.settings, map(&[("index_granularity", "1024")]));
    assert_eq!(
        inner.query,
        "SELECT k, d, count() AS c FROM d.src GROUP BY k, d"
    );

    let refresh = parse_view(
        "CREATE MATERIALIZED VIEW d.mv_dep REFRESH EVERY 30 SECOND OFFSET 5 SECOND RANDOMIZE FOR 5 SECOND \
         DEPENDS ON d.mv_refresh SETTINGS refresh_retries = 3 APPEND TO d.mem (`a` UInt32) \
         DEFINER = default SQL SECURITY DEFINER AS SELECT a FROM d.local_t",
    )
    .unwrap();
    assert_eq!(
        refresh.refresh.as_deref(),
        Some(
            "EVERY 30 SECOND OFFSET 5 SECOND RANDOMIZE FOR 5 SECOND DEPENDS ON d.mv_refresh SETTINGS refresh_retries = 3 APPEND"
        )
    );
    assert_eq!(refresh.to.unwrap().name, "mem");
    assert_eq!(refresh.definer.as_deref(), Some("default"));
    assert_eq!(refresh.sql_security.as_deref(), Some("DEFINER"));

    let commented = parse_view(
        "CREATE MATERIALIZED VIEW d.mv_c TO d.mem (`a` UInt32) COMMENT 'mv comment' AS SELECT a FROM d.local_t",
    )
    .unwrap();
    assert_eq!(commented.comment.as_deref(), Some("mv comment"));
    assert_eq!(commented.query, "SELECT a FROM d.local_t");
}

#[test]
fn views() {
    let view = parse_view(
        "CREATE VIEW d.v_inv (`x` UInt8) SQL SECURITY INVOKER COMMENT 'view c' AS SELECT 1 AS x",
    )
    .unwrap();
    assert!(!view.materialized);
    assert_eq!(view.sql_security.as_deref(), Some("INVOKER"));
    assert_eq!(view.comment.as_deref(), Some("view c"));
    assert_eq!(view.query, "SELECT 1 AS x");
    assert_eq!(view.columns.len(), 1);

    let definer = parse_view(
        "CREATE VIEW d.v_sec (`x` UInt8) DEFINER = `odd user` SQL SECURITY DEFINER AS SELECT 1 AS x",
    )
    .unwrap();
    assert_eq!(definer.definer.as_deref(), Some("odd user"));
}

#[test]
fn dictionaries() {
    let dict = parse_dictionary(
        "CREATE DICTIONARY d.dict (`id` UInt64, `name` String DEFAULT 'x' EXPRESSION upper(b), \
         `parent` UInt64 HIERARCHICAL) PRIMARY KEY id SOURCE(CLICKHOUSE(TABLE 'local_t' DB 'd')) \
         LIFETIME(MIN 0 MAX 300) LAYOUT(HASHED()) SETTINGS(format_csv_allow_single_quotes = 0) COMMENT 'dict'",
    )
    .unwrap();
    assert_eq!(
        dict,
        ParsedDictionary {
            name: Some(ParsedName {
                database: Some("d".into()),
                name: "dict".into()
            }),
            attributes: vec![
                ParsedAttribute {
                    name: "id".into(),
                    data_type: "UInt64".into(),
                    ..ParsedAttribute::default()
                },
                ParsedAttribute {
                    name: "name".into(),
                    data_type: "String".into(),
                    default: Some("'x'".into()),
                    expression: Some("upper(b)".into()),
                    ..ParsedAttribute::default()
                },
                ParsedAttribute {
                    name: "parent".into(),
                    data_type: "UInt64".into(),
                    hierarchical: true,
                    ..ParsedAttribute::default()
                },
            ],
            primary_key: strings(&["id"]),
            source: Some("CLICKHOUSE(TABLE 'local_t' DB 'd')".into()),
            lifetime: Some("MIN 0 MAX 300".into()),
            layout: Some("HASHED()".into()),
            range: None,
            settings: map(&[("format_csv_allow_single_quotes", "0")]),
            comment: Some("dict".into()),
        }
    );
}

#[test]
fn functions() {
    assert_eq!(
        parse_function("CREATE FUNCTION linear_eq AS (x, k, b) -> ((k * x) + b)").unwrap(),
        ParsedFunction {
            name: "linear_eq".into(),
            parameters: strings(&["x", "k", "b"]),
            body: "(k * x) + b".into(),
        }
    );
    assert_eq!(
        parse_function("CREATE FUNCTION f1 AS x -> (x + 1)").unwrap(),
        ParsedFunction {
            name: "f1".into(),
            parameters: strings(&["x"]),
            body: "x + 1".into(),
        }
    );
    assert_eq!(
        parse_function("CREATE FUNCTION `odd-name` AS () -> 42").unwrap(),
        ParsedFunction {
            name: "odd-name".into(),
            parameters: vec![],
            body: "42".into(),
        }
    );
    assert_eq!(
        parse_function("CREATE FUNCTION g AS (a) -> arrayMap(x -> (x * a), [1, 2])")
            .unwrap()
            .body,
        "arrayMap(x -> (x * a), [1, 2])"
    );
}

#[test]
fn redundant_parentheses() {
    assert_eq!(normalize_expression("(today())"), "today()");
    assert_eq!(normalize_expression("((a * 2) + 1)"), "(a * 2) + 1");
    assert_eq!(normalize_expression("(a * 2) + 1"), "(a * 2) + 1");
    assert_eq!(normalize_expression("((a), (b))"), "(a, b)");
    assert_eq!(normalize_expression("(toYYYYMM(d), t)"), "(toYYYYMM(d), t)");
    assert_eq!(normalize_expression("((SELECT 1))"), "(SELECT 1)");
    assert_eq!(normalize_expression("()"), "()");
    assert_eq!(normalize_expression(" x "), "x");
    assert_eq!(
        split_key("((a), (cityHash64(b)), (d))").unwrap(),
        strings(&["a", "cityHash64(b)", "d"])
    );
    assert_eq!(
        normalize_ttl("(d + toIntervalDay(1)) WHERE (a = 1), d + toIntervalDay(2) TO DISK 'x'"),
        "d + toIntervalDay(1) WHERE a = 1, d + toIntervalDay(2) TO DISK 'x'"
    );
    assert_eq!(
        normalize_ttl("(d + toIntervalYear(1))"),
        "d + toIntervalYear(1)"
    );
    assert_eq!(
        normalize_ttl("(d + toIntervalMonth(1)) GROUP BY k SET x = max(x), y = min(y)"),
        "(d + toIntervalMonth(1)) GROUP BY k SET x = max(x), y = min(y)"
    );

    let table = parse_table(
        "CREATE TABLE d.pp (`a` UInt64, `b` String, `d` Date DEFAULT (today()), \
         `t` Date TTL (d + toIntervalDay(1)), INDEX i1 (a) TYPE minmax GRANULARITY 1, \
         INDEX i2 ((a), (b)) TYPE minmax GRANULARITY 1, CONSTRAINT c1 CHECK (a > 0)) \
         ENGINE = MergeTree PARTITION BY (toYYYYMM(d)) PRIMARY KEY ((a), (cityHash64(b))) \
         ORDER BY ((a), (cityHash64(b)), (d)) SAMPLE BY (cityHash64(b)) TTL (d + toIntervalYear(1)) \
         SETTINGS index_granularity = 8192",
    )
    .unwrap();
    let s = &table.storage;
    assert_eq!(s.partition_by.as_deref(), Some("toYYYYMM(d)"));
    assert_eq!(s.primary_key, Some(strings(&["a", "cityHash64(b)"])));
    assert_eq!(s.order_by, Some(strings(&["a", "cityHash64(b)", "d"])));
    assert_eq!(s.sample_by.as_deref(), Some("cityHash64(b)"));
    assert_eq!(s.ttl.as_deref(), Some("d + toIntervalYear(1)"));
    let c = &table.elements.columns;
    assert_eq!(c[2].default, Some(("DEFAULT".into(), "today()".into())));
    assert_eq!(c[3].ttl.as_deref(), Some("d + toIntervalDay(1)"));
    assert_eq!(table.elements.indexes[0].expression, "a");
    assert_eq!(table.elements.indexes[1].expression, "(a, b)");
    assert_eq!(table.elements.constraints[0].expression, "a > 0");
}

#[test]
fn kinds() {
    assert_eq!(
        create_kind("CREATE TABLE a.b (x UInt8) ENGINE = Memory"),
        CreateKind::Table
    );
    assert_eq!(create_kind("CREATE VIEW a.b AS SELECT 1"), CreateKind::View);
    assert_eq!(
        create_kind("CREATE MATERIALIZED VIEW a.b TO a.c AS SELECT 1"),
        CreateKind::MaterializedView
    );
    assert_eq!(
        create_kind("CREATE DICTIONARY a.b (x UInt8)"),
        CreateKind::Dictionary
    );
    assert_eq!(
        create_kind("CREATE LIVE VIEW a.b AS SELECT 1"),
        CreateKind::Other
    );
}

#[test]
fn literals() {
    assert_eq!(unquote_string("'it\\'s'").as_deref(), Some("it's"));
    assert_eq!(unquote_string("'a''b\\\\c'").as_deref(), Some("a'b\\c"));
    assert_eq!(unquote_string("x"), None);
    assert_eq!(unquote_ident("`a\\`b`"), "a`b");
    assert_eq!(unquote_ident("\"a\"\"b\""), "a\"b");
    assert_eq!(unquote_ident("plain"), "plain");
}

#[test]
fn errors() {
    assert!(parse_table("CREATE TABLE a.b (x UInt8)").is_err());
    assert!(parse_table("CREATE TABLE a.b (x UInt8) ENGINE = Memory GARBAGE").is_err());
    assert!(parse_table("CREATE TABLE a.b (x UInt8 ENGINE = Memory").is_err());
    let error = parse_view("CREATE VIEW a.b WHAT").unwrap_err();
    assert!(error.to_string().contains("unexpected `WHAT`"), "{error}");
    assert!(parse_function("CREATE FUNCTION f AS 1").is_err());
}
