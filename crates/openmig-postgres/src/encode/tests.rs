use super::encode;
use crate::decode::decode;
use crate::model::Database;
use crate::settings::{PgSettings, PublicDefaults};
use crate::spec::schema_spec;
use indexmap::IndexMap;
use openmig_core::{EncodeOptions, EncodedFile, SplitBy};
use openmig_hcl::{LoadOptions, load_sources};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn load(files: Vec<(PathBuf, String)>, settings: &PgSettings) -> Database {
    let spec = schema_spec(settings);
    let opts = LoadOptions {
        spec: &spec,
        vars: IndexMap::new(),
    };
    let doc = load_sources(files, &opts).unwrap_or_else(|e| panic!("load:\n{}", e.render(false)));
    decode(&doc, settings)
        .unwrap_or_else(|e| panic!("decode:\n{e}"))
        .model
}

fn round_trip(model: &Database, settings: &PgSettings, split: SplitBy) -> Vec<EncodedFile> {
    let files = encode(model, settings, &EncodeOptions { split });
    let text: Vec<(PathBuf, String)> = files
        .iter()
        .map(|f| (f.path.clone(), f.content.clone()))
        .collect();
    let again = load(text, settings);
    let shown: String = files
        .iter()
        .map(|f| format!("# {}\n{}", f.path.display(), f.content))
        .collect();
    assert_eq!(&again, model, "round trip ({split:?}) differs:\n{shown}");
    files
}

const SCHEMA: &str = r#"
role "web" {}
role "odd.role" {
  member_of = [role.web]
}
schema "my.schema" {
  comment = "dotted"
}
schema "cluster" {}
table "odd" {
  schema = schema["my.schema"]
  column "a b" {
    type = int
  }
}
table "cluster.t" {
  column "id" {
    type = serial
  }
  column "n" {
    type     = int
    identity = { increment = -2, cache = 4 }
  }
}
table "app.with.dot" {
  column "x\"y" {
    type    = text
    default = "it's"
  }
}
sequence "app.items_id_seq" {
  owned_by = table.app.items.column.id
}
table "app.items" {
  column "id" {
    type    = bigint
    null    = false
    default = sql("nextval('app.items_id_seq'::regclass)")
  }
  column "code" {
    type    = char(3)
    default = "abc"
  }
  column "price" {
    type    = numeric(8, 2)
    default = 1.50
  }
  column "at" {
    type    = timestamp(3)
    default = sql("now()")
  }
  primary_key "items_pk" {
    columns = [column.id]
  }
  index {
    key {
      expr = "lower(code)"
    }
  }
}
table "app.nokey" {
  column "a" {
    type = int
  }
  column "b" {
    type = text
  }
}
data "app.nokey" {
  key  = ["a", "b"]
  rows = [{ a = 1, b = "x" }, { a = 2, b = null }]
}
domain "app.label" {
  type    = varchar(20)
  default = "none"
}
composite "app.pair" {
  field "l" {
    type = domain.app.label
  }
  field "r" {
    type = "app.label[]"
  }
}
function "app.f" {
  returns  = trigger
  language = plpgsql
  body     = <<-SQL
    BEGIN
      RETURN NEW;
    END
  SQL
}
function "app.f_int" {
  name = "f"
  arg "x" {
    type = int
  }
  returns  = int
  language = sql
  body     = "SELECT x"
}
function "app.f_text" {
  name = "f"
  arg "x" {
    type = text
  }
  returns  = "text[]"
  language = sql
  body     = "SELECT ARRAY[x]"
  cost     = 0.5
}
view "app.v1" {
  as = "SELECT 1 AS id"
}
view "app.v2" {
  as = "SELECT 2 AS id"
}
trigger "ins" {
  on         = view.app.v1
  instead_of = [INSERT]
  for        = ROW
  execute    = function.app.f
}
trigger "ins_2" {
  name       = "ins"
  on         = view.app.v2
  instead_of = [INSERT]
  for        = ROW
  execute    = function.app.f
}
materialized_view "app.m" {
  as = "SELECT 1 AS n"
  index {
    key {
      expr = "(n + 1)"
    }
  }
}
grant {
  privileges = [USAGE, SELECT]
  on         = [sequence.app.items_id_seq]
  to         = [role.web]
}
grant {
  privileges = [SELECT]
  on         = [table.app.items, view.app.v1, table.cluster.t]
  to         = [role["odd.role"], "outsider"]
}
revoke {
  privileges = [EXECUTE]
  on         = [function.app.f_int, function.app.f_text]
  from       = [PUBLIC]
}
"#;

#[test]
fn decoded_models_round_trip() {
    let settings = PgSettings::default();
    let model = load(
        vec![(PathBuf::from("schema.hcl"), SCHEMA.to_owned())],
        &settings,
    );
    for split in [SplitBy::None, SplitBy::Schema, SplitBy::Kind] {
        round_trip(&model, &settings, split);
    }
    let files = round_trip(&model, &settings, SplitBy::Schema);
    let names: Vec<String> = files.iter().map(|f| f.path.display().to_string()).collect();
    assert_eq!(
        names,
        ["app.hcl", "cluster.hcl", "cluster_2.hcl", "my.schema.hcl"]
    );
    let kinds = round_trip(&model, &settings, SplitBy::Kind);
    let kind_names: Vec<String> = kinds.iter().map(|f| f.path.display().to_string()).collect();
    assert!(kind_names.contains(&"tables.hcl".to_owned()));
    assert!(kind_names.contains(&"grants.hcl".to_owned()));

    let all = round_trip(&model, &settings, SplitBy::None);
    let text = &all[0].content;
    assert!(text.contains("type = serial"), "{text}");
    assert!(text.contains("function \"app.f_integer\""), "{text}");
    assert!(text.contains("execute    = function.app.f\n"), "{text}");
}

#[test]
fn serial_sequences_with_privileges_stay_explicit() {
    let settings = PgSettings::default();
    let model = load(
        vec![(PathBuf::from("schema.hcl"), SCHEMA.to_owned())],
        &settings,
    );
    let files = round_trip(&model, &settings, SplitBy::None);
    let text = &files[0].content;
    assert!(text.contains("sequence \"app.items_id_seq\""), "{text}");
    assert!(
        text.contains("nextval('app.items_id_seq'::regclass)"),
        "{text}"
    );
    assert!(!text.contains("sequence \"cluster.t_id_seq\""), "{text}");
}

#[test]
fn public_defaults_revoke_round_trips() {
    let settings = PgSettings {
        public_defaults: PublicDefaults::Revoke,
        default_schema: "app".into(),
        ..PgSettings::default()
    };
    let model = load(
        vec![(PathBuf::from("schema.hcl"), SCHEMA.to_owned())],
        &settings,
    );
    round_trip(&model, &settings, SplitBy::None);
}

#[test]
fn empty_model() {
    let files = encode(
        &Database::default(),
        &PgSettings::default(),
        &EncodeOptions::default(),
    );
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PathBuf::from("schema.hcl"));
}
