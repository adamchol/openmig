use super::decode;
use crate::model::*;
use crate::settings::{PgSettings, PublicDefaults};
use crate::spec::schema_spec;
use indexmap::IndexMap;
use openmig_core::{DataSync, Error, ObjectKind, ObjectRef, RenameHint};
use openmig_hcl::{LoadOptions, load_sources};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn try_decode_with(
    src: &str,
    settings: &PgSettings,
) -> Result<openmig_core::Desired<Database>, String> {
    let spec = schema_spec(settings);
    let opts = LoadOptions {
        spec: &spec,
        vars: IndexMap::new(),
    };
    let doc = load_sources(vec![(PathBuf::from("schema.hcl"), src.to_owned())], &opts)
        .map_err(|e| e.render(false))?;
    decode(&doc, settings).map_err(|e| match e {
        Error::Schema { rendered } => rendered,
        other => other.to_string(),
    })
}

fn decode_ok(src: &str) -> Database {
    decode_with(src, &PgSettings::default())
}

fn decode_with(src: &str, settings: &PgSettings) -> Database {
    try_decode_with(src, settings)
        .unwrap_or_else(|e| panic!("decode failed:\n{e}"))
        .model
}

fn decode_err(src: &str) -> String {
    match try_decode_with(src, &PgSettings::default()) {
        Ok(_) => panic!("decoding succeeded"),
        Err(e) => e,
    }
}

fn table<'d>(db: &'d Database, schema: &str, name: &str) -> &'d Table {
    db.table(&QualifiedName::new(schema, name))
        .unwrap_or_else(|| panic!("no table {schema}.{name}"))
}

fn acl(items: &[(&str, Privilege, bool)]) -> Acl {
    items
        .iter()
        .map(|(g, p, grantable)| AclItem {
            grantee: (*g).to_owned(),
            privilege: *p,
            grantable: *grantable,
        })
        .collect()
}

#[test]
fn implicit_schemas() {
    let db = decode_ok(
        r#"
table "app.users" {
  column "id" {
    type = bigint
  }
}
"#,
    );
    assert_eq!(db.schemas.keys().collect::<Vec<_>>(), ["app", "public"]);
    let public = &db.schemas["public"];
    assert_eq!(public.comment.as_deref(), Some("standard public schema"));
    assert_eq!(public.acl, acl(&[(PUBLIC, Privilege::Usage, false)]));
    assert_eq!(db.schemas["app"].comment, None);
    assert!(db.schemas["app"].acl.is_empty());

    let settings = PgSettings {
        default_schema: "app".into(),
        public_defaults: PublicDefaults::Revoke,
        ..PgSettings::default()
    };
    let db = decode_with(
        "table \"users\" {\n  column \"id\" {\n    type = int\n  }\n}\n",
        &settings,
    );
    assert!(db.schemas["app"].tables.contains_key("users"));
    assert!(db.schemas["public"].acl.is_empty());

    let db = decode_ok(
        "schema \"public\" {\n  comment = \"\"\n}\nschema \"audit\" {\n  comment = \"logs\"\n}\n",
    );
    assert_eq!(db.schemas["public"].comment, None);
    assert_eq!(db.schemas["audit"].comment.as_deref(), Some("logs"));
}

#[test]
fn columns_and_types() {
    let db = decode_ok(
        r#"
enum "app.status" {
  values = ["active", "disabled"]
}
table "app.users" {
  column "id" {
    type = uuid
    default = sql("gen_random_uuid()")
  }
  column "name" {
    type = varchar(100)
    null = false
    default = "it's"
    collate = "C"
    comment = "display name"
  }
  column "status" {
    type    = enum.app.status
    default = "active"
  }
  column "tags" {
    type    = "text[]"
    default = "{}"
  }
  column "score" {
    type    = double_precision
    default = 0
  }
  column "at" {
    type = timestamptz(3)
  }
  column "price" {
    type = numeric(10, 2)
  }
  column "flag" {
    type    = bool
    default = false
  }
  column "secret" {
    type        = text
    storage     = EXTERNAL
    compression = lz4
  }
  column "full" {
    type      = text
    generated = "name || '!'"
  }
  column "area" {
    type      = int
    generated = { expr = "1 + 1", virtual = true }
  }
  primary_key {
    columns = [column.id]
  }
}
"#,
    );
    let t = table(&db, "app", "users");
    let col = |n: &str| t.column(n).unwrap();
    assert_eq!(col("id").data_type, "uuid");
    assert!(col("id").not_null, "primary key columns are NOT NULL");
    assert_eq!(col("id").default.as_deref(), Some("gen_random_uuid()"));
    assert_eq!(col("name").data_type, "character varying(100)");
    assert_eq!(
        col("name").default.as_deref(),
        Some("'it''s'::character varying")
    );
    assert_eq!(col("name").collation.as_deref(), Some("\"C\""));
    assert_eq!(col("name").comment.as_deref(), Some("display name"));
    assert!(col("name").not_null);
    assert_eq!(col("status").data_type, "app.status");
    assert_eq!(
        col("status").default.as_deref(),
        Some("'active'::app.status")
    );
    assert_eq!(col("tags").default.as_deref(), Some("'{}'::text[]"));
    assert_eq!(col("score").data_type, "double precision");
    assert_eq!(col("score").default.as_deref(), Some("0"));
    assert_eq!(col("at").data_type, "timestamp(3) with time zone");
    assert_eq!(col("price").data_type, "numeric(10,2)");
    assert_eq!(col("flag").data_type, "boolean");
    assert_eq!(col("flag").default.as_deref(), Some("false"));
    assert_eq!(col("secret").storage, Some(Storage::External));
    assert_eq!(col("secret").compression.as_deref(), Some("lz4"));
    assert_eq!(
        col("full").generated,
        Some(Generated {
            expr: "name || '!'".into(),
            kind: GeneratedKind::Stored
        })
    );
    assert_eq!(
        col("area").generated.as_ref().unwrap().kind,
        GeneratedKind::Virtual
    );
    assert!(!col("tags").not_null);
    let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names[..3], ["id", "name", "status"]);
    let pk = t.primary_key.as_ref().unwrap();
    assert_eq!(pk.name, "users_pkey");
    assert_eq!(pk.columns, ["id"]);
    let index = &t.indexes["users_pkey"];
    assert!(index.constraint && index.unique);
    assert_eq!(index.keys, [IndexKey::column("id")]);
}

#[test]
fn identity_and_serial() {
    let db = decode_ok(
        r#"
table "app.t" {
  column "a" {
    type     = bigint
    identity = ALWAYS
  }
  column "b" {
    type     = smallint
    identity = { generated = BY_DEFAULT, min = -5, start = -5, max = 500, cache = 3, cycle = true }
  }
  column "c" {
    type     = int
    identity = { increment = -1 }
  }
  column "d" {
    type = bigserial
  }
}
table "audit.log" {
  column "id" {
    type = serial
  }
}
"#,
    );
    let t = table(&db, "app", "t");
    let a = t.column("a").unwrap();
    assert!(a.not_null);
    assert_eq!(
        a.identity,
        Some(Identity {
            generation: IdentityGeneration::Always,
            sequence: SequenceOptions::defaults_for("bigint"),
        })
    );
    assert_eq!(
        t.column("b").unwrap().identity.as_ref().unwrap().sequence,
        SequenceOptions {
            start: -5,
            increment: 1,
            min_value: -5,
            max_value: 500,
            cache: 3,
            cycle: true,
        }
    );
    let c = &t.column("c").unwrap().identity.as_ref().unwrap();
    assert_eq!(c.generation, IdentityGeneration::ByDefault);
    assert_eq!(
        (c.sequence.start, c.sequence.min_value, c.sequence.max_value),
        (-1, i64::from(i32::MIN), -1)
    );
    let d = t.column("d").unwrap();
    assert_eq!(d.data_type, "bigint");
    assert!(d.not_null);
    assert_eq!(
        d.default.as_deref(),
        Some("nextval('app.t_d_seq'::regclass)")
    );
    let seq = &db.schemas["app"].sequences["t_d_seq"];
    assert_eq!(seq.data_type, "bigint");
    assert_eq!(
        seq.owned_by,
        Some(ColumnRef {
            schema: "app".into(),
            table: "t".into(),
            column: "d".into()
        })
    );
    let log = table(&db, "audit", "log");
    assert_eq!(log.column("id").unwrap().data_type, "integer");
    assert_eq!(
        db.schemas["audit"].sequences["log_id_seq"].options,
        SequenceOptions::defaults_for("integer")
    );
}

#[test]
fn constraints() {
    let db = decode_ok(
        r#"
table "app.orgs" {
  column "id" {
    type = bigint
  }
  column "slug" {
    type = text
  }
  primary_key "orgs_pk" {
    columns = ["id"]
  }
  unique "orgs_slug_key" {
    columns            = [column.slug]
    deferrable         = true
    initially_deferred = true
    nulls_distinct     = false
    include            = ["id"]
    with               = { fillfactor = 90 }
  }
}
table "app.users" {
  column "id" {
    type = bigint
  }
  column "org_id" {
    type = bigint
  }
  column "score" {
    type = int
  }
  column "during" {
    type = tstzrange
  }
  foreign_key "users_org_fk" {
    columns    = [column.org_id]
    references = [table.app.orgs.column.id]
    on_delete  = CASCADE
    on_update  = "set null"
    match      = FULL
    not_valid  = true
    comment    = "owner"
  }
  foreign_key "users_self_fk" {
    columns    = [column.org_id]
    references = [column.id]
  }
  check "users_score_check" {
    expr       = "score >= 0"
    no_inherit = true
  }
  exclude "users_no_overlap" {
    using = gist
    key {
      column   = column.org_id
      operator = "="
    }
    key {
      column   = column.during
      operator = "&&"
    }
    where = "id > 0"
  }
}
"#,
    );
    let orgs = table(&db, "app", "orgs");
    assert_eq!(orgs.primary_key.as_ref().unwrap().name, "orgs_pk");
    let u = &orgs.uniques["orgs_slug_key"];
    assert_eq!(
        (u.deferrable, u.initially_deferred, u.nulls_distinct),
        (true, true, false)
    );
    assert_eq!(u.include, ["id"]);
    assert_eq!(u.storage_params["fillfactor"], "90");
    let index = &orgs.indexes["orgs_slug_key"];
    assert!(index.constraint && !index.nulls_distinct);
    let users = table(&db, "app", "users");
    let fk = &users.foreign_keys["users_org_fk"];
    assert_eq!(fk.ref_table, QualifiedName::new("app", "orgs"));
    assert_eq!(fk.ref_columns, ["id"]);
    assert_eq!(fk.on_delete, FkAction::Cascade);
    assert_eq!(fk.on_update, FkAction::SetNull);
    assert_eq!(fk.match_type, FkMatch::Full);
    assert!(!fk.validated && fk.enforced);
    assert_eq!(
        users.foreign_keys["users_self_fk"].ref_table,
        QualifiedName::new("app", "users")
    );
    let check = &users.checks["users_score_check"];
    assert!(check.no_inherit && check.validated);
    let x = &users.exclusions["users_no_overlap"];
    assert_eq!(x.method, "gist");
    assert_eq!(x.elements[1].operator, "&&");
    assert_eq!(x.predicate.as_deref(), Some("id > 0"));
    assert!(users.indexes["users_no_overlap"].constraint);
}

#[test]
fn indexes_and_generated_names() {
    let db = decode_ok(
        r#"
table "app.users" {
  column "id" {
    type = bigint
  }
  column "email" {
    type = text
  }
  column "tags" {
    type = "text[]"
  }
  primary_key {
    columns = [column.id]
  }
  index {
    columns = [column.email]
  }
  index {
    columns = [column.email]
    unique  = true
  }
  index {
    key {
      expr    = "lower(email)"
      opclass = text_pattern_ops
      desc    = true
      nulls   = LAST
    }
    where = "id > 0"
  }
  index "users_tags_idx" {
    columns        = [column.tags]
    using          = gin
    comment        = "tags"
    nulls_distinct = false
  }
  index "users_mixed_idx" {
    key {
      column  = column.email
      collate = "C"
      nulls   = FIRST
    }
    key {
      expr = "(id + 1)"
    }
    include = ["tags"]
    with    = { fillfactor = 70 }
  }
}
"#,
    );
    let t = table(&db, "app", "users");
    let names: Vec<&str> = t.indexes.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "users_email_idx",
            "users_email_idx1",
            "users_lower_idx",
            "users_mixed_idx",
            "users_pkey",
            "users_tags_idx"
        ]
    );
    assert!(t.indexes["users_email_idx1"].unique);
    let lower = &t.indexes["users_lower_idx"];
    assert_eq!(
        lower.keys,
        [IndexKey {
            expr: KeyExpr::Expression("lower(email)".into()),
            opclass: Some("text_pattern_ops".into()),
            collation: None,
            descending: true,
            nulls: Some(NullsOrder::Last),
        }]
    );
    assert_eq!(lower.predicate.as_deref(), Some("id > 0"));
    let tags = &t.indexes["users_tags_idx"];
    assert_eq!(tags.method, "gin");
    assert!(!tags.nulls_distinct);
    let mixed = &t.indexes["users_mixed_idx"];
    assert_eq!(mixed.keys[0].collation.as_deref(), Some("\"C\""));
    assert_eq!(mixed.keys[0].nulls, Some(NullsOrder::First));
    assert_eq!(mixed.keys[1].expr, KeyExpr::Expression("(id + 1)".into()));
    assert_eq!(mixed.include, ["tags"]);
    assert_eq!(mixed.storage_params["fillfactor"], "70");
}

#[test]
fn table_options_policies_and_triggers() {
    let db = decode_ok(
        r#"
role "web" {}
function "app.touch" {
  returns  = trigger
  language = plpgsql
  body     = "BEGIN RETURN NEW; END"
}
table "app.parent" {
  column "id" {
    type = int
  }
  column "at" {
    type = timestamptz
  }
  partition_by = "RANGE (at)"
}
table "app.part" {
  column "id" {
    type = int
  }
  column "at" {
    type = timestamptz
  }
  partition_of    = table.app.parent
  partition_bound = "DEFAULT"
}
table "app.users" {
  column "id" {
    type = bigint
  }
  column "name" {
    type = text
  }
  primary_key {
    columns = [column.id]
  }
  rls              = { enabled = true, force = true }
  replica_identity = "users_pkey"
  unlogged         = true
  storage_params   = { autovacuum_enabled = false, fillfactor = 70 }
  inherits         = [table.app.parent]
  comment          = "people"
  policy "users_select" {
    for   = SELECT
    to    = [role.web, PUBLIC, "worker"]
    using = "true"
  }
  policy "users_admin" {
    as    = RESTRICTIVE
    for   = DELETE
    check = "false"
  }
  trigger "users_touch" {
    before    = [UPDATE]
    update_of = [column.name]
    for       = ROW
    when      = "old.* IS DISTINCT FROM new.*"
    execute   = function.app.touch
    args      = ["a", "b'c", 3]
    enabled   = REPLICA
    comment   = "touch"
  }
  trigger "users_check" {
    after              = [INSERT, DELETE]
    for                = ROW
    execute            = function.app.touch
    constraint         = true
    deferrable         = true
    initially_deferred = true
  }
}
policy "users_insert" {
  on    = table.app.users
  for   = INSERT
  to    = [role.web]
  check = "true"
}
"#,
    );
    let users = table(&db, "app", "users");
    assert!(users.rls_enabled && users.rls_forced && users.unlogged);
    assert_eq!(
        users.replica_identity,
        ReplicaIdentity::Index("users_pkey".into())
    );
    assert_eq!(users.storage_params["autovacuum_enabled"], "false");
    assert_eq!(users.inherits, [QualifiedName::new("app", "parent")]);
    let select = &users.policies["users_select"];
    assert_eq!(select.command, PolicyCommand::Select);
    assert_eq!(select.roles, ["PUBLIC", "web", "worker"]);
    assert!(select.permissive);
    let admin = &users.policies["users_admin"];
    assert!(!admin.permissive);
    assert_eq!(admin.roles, ["PUBLIC"]);
    assert_eq!(
        users.policies["users_insert"].check.as_deref(),
        Some("true")
    );
    let touch = &users.triggers["users_touch"];
    assert_eq!(touch.timing, TriggerTiming::Before);
    assert_eq!(
        touch.events.iter().copied().collect::<Vec<_>>(),
        [TriggerEvent::Update]
    );
    assert_eq!(touch.update_of, ["name"]);
    assert_eq!(touch.function, QualifiedName::new("app", "touch"));
    assert_eq!(touch.args, ["a", "b'c", "3"]);
    assert_eq!(touch.enabled, TriggerEnabled::Replica);
    let check = &users.triggers["users_check"];
    assert_eq!(
        check.constraint,
        Some(ConstraintTrigger {
            deferrable: true,
            initially_deferred: true,
            from: None
        })
    );
    assert_eq!(
        table(&db, "app", "parent").partition_by.as_deref(),
        Some("RANGE (at)")
    );
    assert_eq!(
        table(&db, "app", "part").partition_of,
        Some(PartitionOf {
            parent: QualifiedName::new("app", "parent"),
            bound: "DEFAULT".into()
        })
    );
    assert_eq!(db.external_roles.iter().collect::<Vec<_>>(), ["worker"]);
}

#[test]
fn views() {
    let db = decode_ok(
        r#"
view "app.active" {
  as = <<-SQL
    SELECT 1 AS one;
  SQL
  security_barrier = true
  security_invoker = true
  check_option     = CASCADED
  comment          = "active"
  column "one" {
    comment = "the number"
  }
}
materialized_view "app.stats" {
  as        = "SELECT 1 AS n"
  with_data = false
  index {
    columns = ["n"]
    unique  = true
  }
}
trigger "active_insert" {
  on         = view.app.active
  instead_of = [INSERT]
  for        = ROW
  execute    = "app.ins"
}
"#,
    );
    let app = &db.schemas["app"];
    let v = &app.views["active"];
    assert_eq!(v.query, "SELECT 1 AS one");
    assert!(v.security_barrier && v.security_invoker);
    assert_eq!(v.check_option, Some(ViewCheckOption::Cascaded));
    assert_eq!(v.columns[0].comment.as_deref(), Some("the number"));
    assert_eq!(
        v.triggers["active_insert"].function,
        QualifiedName::new("app", "ins")
    );
    let m = &app.materialized_views["stats"];
    assert!(!m.with_data);
    assert!(m.indexes["stats_n_idx"].unique);
}

#[test]
fn functions() {
    let db = decode_ok(
        r#"
enum "app.status" {
  values = ["active"]
}
table "app.users" {
  column "id" {
    type = uuid
  }
}
function "app.add" {
  arg "a" {
    type = int
  }
  arg "b" {
    type    = integer
    default = 1
  }
  returns    = int
  language   = SQL
  body       = "RETURN a + b"
  volatility = IMMUTABLE
  strict     = true
  leakproof  = true
  comment    = "adds"
}
function "app.add_numeric" {
  name = "add"
  arg "a" {
    type = numeric(10, 2)
  }
  arg "b" {
    type = numeric
  }
  returns  = numeric
  language = sql
  body     = "SELECT a + b"
}
function "app.org_users" {
  arg "p_org" {
    type = bigint
  }
  arg "p_status" {
    type    = "app.status[]"
    mode    = VARIADIC
    default = "{active}"
  }
  returns_table {
    column "id" {
      type = uuid
    }
  }
  language = sql
  body     = "BEGIN ATOMIC SELECT 1; END"
  rows     = 50
  cost     = 20
  parallel = SAFE
  set      = { search_path = "pg_catalog, app", work_mem = "64MB" }
  security = DEFINER
}
function "app.split" {
  arg "full" {
    type = text
  }
  arg "first" {
    type = text
    mode = OUT
  }
  arg "last" {
    type = text
    mode = OUT
  }
  language = sql
  body     = "SELECT 'a', 'b'"
}
function "app.of_org" {
  arg {
    type = bigint
  }
  returns  = table.app.users
  setof    = true
  language = sql
  body     = "SELECT * FROM app.users"
}
procedure "app.archive" {
  arg "p_before" {
    type = timestamptz
  }
  arg "p_count" {
    type    = int
    mode    = INOUT
    default = 0
  }
  language = plpgsql
  body     = "BEGIN END"
}
"#,
    );
    let app = &db.schemas["app"];
    assert_eq!(
        app.functions.keys().collect::<Vec<_>>(),
        [
            "add(integer, integer)",
            "add(numeric, numeric)",
            "of_org(bigint)",
            "org_users(bigint, app.status[])",
            "split(text)"
        ]
    );
    let add = &app.functions["add(integer, integer)"];
    assert_eq!(add.language, "sql");
    assert_eq!(add.body, FunctionBody::Standard("RETURN a + b".into()));
    assert_eq!(add.args[1].default.as_deref(), Some("1"));
    assert_eq!(add.volatility, Volatility::Immutable);
    assert!(add.strict && add.leakproof);
    assert_eq!((add.cost.as_str(), add.rows.as_str()), ("100", "0"));
    assert_eq!(add.acl, acl(&[(PUBLIC, Privilege::Execute, false)]));
    let numeric = &app.functions["add(numeric, numeric)"];
    assert_eq!(numeric.name, "add");
    assert_eq!(numeric.args[0].data_type, "numeric");
    assert_eq!(
        numeric.body,
        FunctionBody::Definition("SELECT a + b".into())
    );
    let org = &app.functions["org_users(bigint, app.status[])"];
    assert_eq!(org.args[1].mode, ArgMode::Variadic);
    assert_eq!(
        org.args[1].default.as_deref(),
        Some("'{active}'::app.status[]")
    );
    assert_eq!(
        org.returns,
        Some(Returns::Table(vec![ReturnColumn {
            name: "id".into(),
            data_type: "uuid".into()
        }]))
    );
    assert!(matches!(org.body, FunctionBody::Standard(_)));
    assert_eq!((org.cost.as_str(), org.rows.as_str()), ("20", "50"));
    assert_eq!(org.config["search_path"], "pg_catalog, app");
    assert!(org.security_definer);
    assert_eq!(org.parallel, Parallel::Safe);
    let split = &app.functions["split(text)"];
    assert_eq!(
        split.returns,
        Some(Returns::Type {
            data_type: "record".into(),
            setof: false
        })
    );
    let of_org = &app.functions["of_org(bigint)"];
    assert_eq!(of_org.args[0].name, None);
    assert_eq!(
        of_org.returns,
        Some(Returns::Type {
            data_type: "app.users".into(),
            setof: true
        })
    );
    assert_eq!(of_org.rows, "1000");
    let archive = &app.procedures["archive(timestamp with time zone, integer)"];
    assert_eq!(archive.returns, None);
    assert_eq!(archive.args[1].mode, ArgMode::InOut);
    assert_eq!(archive.acl, acl(&[(PUBLIC, Privilege::Execute, false)]));
}

#[test]
fn types_sequences_cluster_objects() {
    let db = decode_ok(
        r#"
extension "citext" {
  version = "1.6"
  comment = "ci"
}
extension "pgcrypto" {
  schema = schema.ext
}
extension "plpgsql" {}
schema "ext" {}
composite "app.address" {
  field "street" {
    type = text
  }
  field "city" {
    type    = text
    collate = "C"
  }
}
domain "app.money" {
  type    = numeric(12, 2)
  null    = false
  default = 0
  check "positive" {
    expr    = "VALUE >= 0"
    comment = "no debt"
  }
}
domain "app.email" {
  type = "public.citext"
  check "email_at" {
    expr      = "VALUE ~ '@'"
    not_valid = true
  }
}
sequence "app.invoice_no" {
  type      = int
  start     = 1000
  increment = 10
  min       = 1000
  max       = 999999
  cache     = 5
  cycle     = true
  owned_by  = table.app.orgs.column.invoice
  comment   = "numbers"
}
table "app.orgs" {
  column "invoice" {
    type = int
  }
}
role "admin" {
  createrole = true
}
role "worker" {
  inherit   = false
  createdb  = true
  bypassrls = true
  member_of = [{ role = role.admin, admin = true }, "pg_read_all_data"]
}
role "web" {
  login            = true
  connection_limit = 10
  valid_until      = "2030-01-01 00:00:00+00"
  member_of        = [role.worker, "outsider"]
  comment          = "web app"
}
function "app.on_ddl" {
  returns  = event_trigger
  language = plpgsql
  body     = "BEGIN END"
}
event_trigger "ddl" {
  on      = ddl_command_end
  tags    = ["CREATE TABLE", "ALTER TABLE"]
  execute = function.app.on_ddl
  enabled = ALWAYS
  comment = "hook"
}
"#,
    );
    assert_eq!(
        db.extensions.keys().collect::<Vec<_>>(),
        ["citext", "pgcrypto"]
    );
    assert_eq!(db.extensions["citext"].schema, "public");
    assert_eq!(db.extensions["citext"].version, "1.6");
    assert_eq!(db.extensions["pgcrypto"].schema, "ext");
    assert_eq!(db.extensions["pgcrypto"].version, "");
    let app = &db.schemas["app"];
    let address = &app.composites["address"];
    assert_eq!(address.fields[1].collation.as_deref(), Some("\"C\""));
    assert_eq!(address.acl, acl(&[(PUBLIC, Privilege::Usage, false)]));
    let money = &app.domains["money"];
    assert_eq!(money.data_type, "numeric(12,2)");
    assert!(money.not_null);
    assert_eq!(money.default.as_deref(), Some("0"));
    assert_eq!(money.checks["positive"].comment.as_deref(), Some("no debt"));
    assert!(!app.domains["email"].checks["email_at"].validated);
    let seq = &app.sequences["invoice_no"];
    assert_eq!(seq.data_type, "integer");
    assert_eq!(
        seq.options,
        SequenceOptions {
            start: 1000,
            increment: 10,
            min_value: 1000,
            max_value: 999999,
            cache: 5,
            cycle: true
        }
    );
    assert_eq!(seq.owned_by.as_ref().unwrap().column, "invoice");
    assert_eq!(
        db.roles["worker"].member_of["admin"],
        Membership {
            admin: true,
            inherit: false,
            set: true
        }
    );
    assert_eq!(
        db.roles["web"].member_of["worker"],
        Membership {
            admin: false,
            inherit: true,
            set: true
        }
    );
    assert_eq!(db.roles["web"].connection_limit, 10);
    assert_eq!(db.external_roles.iter().collect::<Vec<_>>(), ["outsider"]);
    let et = &db.event_triggers["ddl"];
    assert_eq!(et.tags, ["ALTER TABLE", "CREATE TABLE"]);
    assert_eq!(et.enabled, TriggerEnabled::Always);
    assert_eq!(et.function, QualifiedName::new("app", "on_ddl"));
}

#[test]
fn grants_and_revokes() {
    let src = r#"
role "web" {}
schema "app" {}
table "app.users" {
  column "id" {
    type = bigint
  }
  column "email" {
    type = text
  }
}
view "app.v" {
  as = "SELECT 1 AS id"
}
function "app.f" {
  returns  = int
  language = sql
  body     = "SELECT 1"
}
enum "app.e" {
  values = ["a"]
}
sequence "app.s" {}
grant {
  privileges = [USAGE]
  on         = [schema.app, sequence.app.s]
  to         = [role.web, "ext"]
}
grant {
  privileges        = [SELECT, INSERT]
  on                = [table.app.users]
  to                = [role.web]
  with_grant_option = true
}
grant {
  privileges = [SELECT]
  on         = [table.app.users]
  to         = [role.web]
}
grant {
  privileges = [ALL]
  on         = [table.app.users]
  to         = ["admin"]
}
grant {
  privileges = [UPDATE]
  on         = [table.app.users, view.app.v]
  columns    = ["id"]
  to         = [PUBLIC]
}
revoke {
  privileges = [EXECUTE]
  on         = [function.app.f]
  from       = [PUBLIC]
}
revoke {
  privileges = [ALL]
  on         = [enum.app.e]
  from       = [PUBLIC]
}
grant {
  privileges = [EXECUTE]
  on         = [function.app.f]
  to         = [role.web]
}
"#;
    let db = decode_ok(src);
    let app = &db.schemas["app"];
    assert_eq!(
        app.acl,
        acl(&[
            ("ext", Privilege::Usage, false),
            ("web", Privilege::Usage, false)
        ])
    );
    assert_eq!(app.sequences["s"].acl.len(), 2);
    let users = &app.tables["users"];
    let web: Vec<_> = users.acl.iter().filter(|i| i.grantee == "web").collect();
    assert_eq!(web.len(), 2);
    assert!(web.iter().all(|i| i.grantable));
    assert_eq!(users.acl.iter().filter(|i| i.grantee == "admin").count(), 8);
    assert_eq!(
        users.column("id").unwrap().acl,
        acl(&[(PUBLIC, Privilege::Update, false)])
    );
    assert_eq!(
        app.views["v"].columns[0].acl,
        acl(&[(PUBLIC, Privilege::Update, false)])
    );
    assert_eq!(
        app.functions["f()"].acl,
        acl(&[("web", Privilege::Execute, false)])
    );
    assert!(app.enums["e"].acl.is_empty());
    assert_eq!(
        db.external_roles.iter().collect::<Vec<_>>(),
        ["admin", "ext"]
    );

    let revoke = PgSettings {
        public_defaults: PublicDefaults::Revoke,
        ..PgSettings::default()
    };
    let db = decode_with(src, &revoke);
    assert_eq!(db.schemas["public"].acl, Acl::new());
    assert_eq!(db.schemas["app"].functions["f()"].acl.len(), 1);
}

#[test]
fn data_rows() {
    let db = decode_ok(
        r#"
table "app.statuses" {
  column "code" {
    type = text
  }
  column "label" {
    type = text
  }
  column "sort" {
    type = int
  }
  column "active" {
    type = bool
  }
  primary_key {
    columns = [column.code]
  }
}
data "app.statuses" {
  rows = [
    { code = "a", label = "Active", sort = 1, active = true },
    { code = "d", label = "Disabled", sort = null },
  ]
  mode = insert
}
data "app.statuses" {
  rows = [{ code = "x", label = "X", sort = 1.50 }]
}
"#,
    );
    let data = &db.data[&QualifiedName::new("app", "statuses")];
    assert_eq!(data.key, ["code"]);
    assert_eq!(data.mode, Some(DataSync::Insert));
    assert_eq!(data.rows.len(), 3);
    let a = &data.rows[&vec![Some("a".to_owned())]];
    assert_eq!(a["sort"].as_deref(), Some("1"));
    assert_eq!(a["active"].as_deref(), Some("true"));
    assert_eq!(data.rows[&vec![Some("d".to_owned())]]["sort"], None);
    assert_eq!(
        data.rows[&vec![Some("x".to_owned())]]["sort"].as_deref(),
        Some("1.50")
    );
}

#[test]
fn names_renames_origins_and_dependencies() {
    let src = r#"
table "app.people" {
  name         = "users"
  renamed_from = "old.persons"
  column "full_name" {
    type         = text
    renamed_from = "name"
  }
}
table "accounts" {
  schema       = schema.billing
  renamed_from = "acct"
  column "id" {
    type = int
  }
  depends_on = [table.app.people]
}
schema "billing" {}
"#;
    let desired = try_decode_with(src, &PgSettings::default()).unwrap();
    let db = &desired.model;
    assert!(db.schemas["app"].tables.contains_key("users"));
    assert!(db.schemas["billing"].tables.contains_key("accounts"));
    assert_eq!(
        desired.hints.0,
        [
            RenameHint {
                kind: ObjectKind::Table,
                from: vec!["old".into(), "persons".into()],
                to: vec!["app".into(), "users".into()],
            },
            RenameHint {
                kind: ObjectKind::Column,
                from: vec!["app".into(), "users".into(), "name".into()],
                to: vec!["app".into(), "users".into(), "full_name".into()],
            },
            RenameHint {
                kind: ObjectKind::Table,
                from: vec!["billing".into(), "acct".into()],
                to: vec!["billing".into(), "accounts".into()],
            },
        ]
    );
    let column = ObjectRef::new(ObjectKind::Column, ["app", "users", "full_name"]);
    let span = desired.origins[&column];
    assert_eq!(&src[span.start..span.end], "column \"full_name\"");
    let (_, line, _) = desired.locate(&column).unwrap();
    assert_eq!(line, 5);
    assert!(
        desired
            .origins
            .contains_key(&ObjectRef::new(ObjectKind::Schema, ["app"]))
    );
    assert_eq!(
        db.dependencies,
        [Dependency {
            from: ObjectRef::new(ObjectKind::Table, ["billing", "accounts"]),
            to: ObjectRef::new(ObjectKind::Table, ["app", "users"]),
        }]
    );
}

#[test]
fn for_each_instances_use_name() {
    let db = decode_ok(
        r#"
table "app.t" {
  for_each = ["a", "b"]
  name     = "t_${each.value}"
  column "id" {
    type = int
  }
  column "x" {
    for_each = ["c1", "c2"]
    name     = each.value
    type     = text
  }
}
"#,
    );
    let app = &db.schemas["app"];
    assert_eq!(app.tables.keys().collect::<Vec<_>>(), ["t_a", "t_b"]);
    assert_eq!(app.tables["t_a"].columns.len(), 3);
}

fn snapshot_error(name: &str, src: &str) {
    insta::assert_snapshot!(name, decode_err(src));
}

#[test]
fn error_unknown_attribute_and_column() {
    snapshot_error(
        "unknown_attribute_and_column",
        r#"
table "app.users" {
  column "id" {
    type     = bigint
    nullable = false
  }
  index "users_email_idx" {
    columns = ["emial"]
  }
}
"#,
    );
}

#[test]
fn error_conflicting_column_options() {
    snapshot_error(
        "conflicting_column_options",
        r#"
table "app.t" {
  column "id" {
    type     = int
    identity = ALWAYS
    default  = 1
  }
  column "s" {
    type    = serial
    default = 2
  }
}
"#,
    );
}

#[test]
fn error_duplicate_signature() {
    snapshot_error(
        "duplicate_signature",
        r#"
function "app.f" {
  arg "a" {
    type = int
  }
  returns  = int
  language = sql
  body     = "SELECT a"
}
function "app.f_again" {
  name = "f"
  arg "b" {
    type = int4
  }
  returns  = int
  language = sql
  body     = "SELECT b"
}
"#,
    );
}

#[test]
fn error_for_each_without_name() {
    snapshot_error(
        "for_each_without_name",
        r#"
table "app.t" {
  for_each = ["a", "b"]
  column "id" {
    type = int
  }
}
"#,
    );
}

#[test]
fn error_bad_references() {
    snapshot_error(
        "bad_references",
        r#"
table "app.a" {
  column "id" {
    type = int
  }
}
table "app.b" {
  column "id" {
    type = int
  }
  column "x" {
    type = int
  }
  foreign_key "b_fk" {
    columns    = [column.id, column.x]
    references = [table.app.a.column.id, column.x]
  }
  primary_key {
    columns = [table.app.a.column.id]
  }
}
"#,
    );
}

#[test]
fn error_data_rows() {
    snapshot_error(
        "data_rows",
        r#"
table "app.t" {
  column "id" {
    type = int
  }
  column "v" {
    type = text
  }
}
table "app.k" {
  column "id" {
    type = int
  }
  column "at" {
    type = timestamptz
  }
  column "x" {
    type = int
  }
  primary_key {
    columns = [column.id]
  }
}
data "app.t" {
  rows = [{ id = 1 }]
}
data "app.k" {
  rows = [
    { id = 1, at = sql("now()") },
    { id = 2, y = 1 },
    { id = 3 },
    { id = 3, x = 1 },
    { x = 4 },
  ]
}
"#,
    );
}

#[test]
fn error_trigger_and_grant() {
    snapshot_error(
        "trigger_and_grant",
        r#"
table "app.t" {
  column "id" {
    type = int
  }
  trigger "t_x" {
    before  = [INSERT]
    after   = [UPDATE]
    execute = "app.f"
  }
}
grant {
  privileges = [EXECUTE]
  on         = [table.app.t]
  to         = [web]
}
"#,
    );
}

#[test]
fn error_namespace_clash() {
    snapshot_error(
        "namespace_clash",
        r#"
table "app.users" {
  column "id" {
    type = int
  }
  index "users" {
    columns = ["id"]
  }
}
"#,
    );
}
