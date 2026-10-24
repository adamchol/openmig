//! The PostgreSQL block vocabulary (see `docs/spec/hcl.md`).

use crate::settings::PgSettings;
use openmig_hcl::{BlockSpec, LabelSpec, SchemaSpec};

const KEY: &[BlockSpec] = &[BlockSpec::new("key", LabelSpec::None)];

const INDEX: BlockSpec = BlockSpec::new("index", LabelSpec::OptionalOne)
    .referenceable()
    .nested(KEY);

const TABLE: &[BlockSpec] = &[
    BlockSpec::new("column", LabelSpec::One).referenceable(),
    BlockSpec::new("primary_key", LabelSpec::OptionalOne),
    BlockSpec::new("foreign_key", LabelSpec::One).referenceable(),
    BlockSpec::new("unique", LabelSpec::One).referenceable(),
    BlockSpec::new("check", LabelSpec::One).referenceable(),
    BlockSpec::new("exclude", LabelSpec::One).nested(KEY),
    INDEX,
    BlockSpec::new("policy", LabelSpec::One),
    BlockSpec::new("trigger", LabelSpec::One),
];

const VIEW: &[BlockSpec] = &[BlockSpec::new("column", LabelSpec::One)];

const MATERIALIZED_VIEW: &[BlockSpec] = &[BlockSpec::new("column", LabelSpec::One), INDEX];

const RETURNS_TABLE: &[BlockSpec] = &[BlockSpec::new("column", LabelSpec::One)];

const FUNCTION: &[BlockSpec] = &[
    BlockSpec::new("arg", LabelSpec::OptionalOne),
    BlockSpec::new("returns_table", LabelSpec::None).nested(RETURNS_TABLE),
];

const PROCEDURE: &[BlockSpec] = &[BlockSpec::new("arg", LabelSpec::OptionalOne)];

const COMPOSITE: &[BlockSpec] = &[BlockSpec::new("field", LabelSpec::One)];

const DOMAIN: &[BlockSpec] = &[BlockSpec::new("check", LabelSpec::One)];

const BLOCKS: &[BlockSpec] = &[
    BlockSpec::new("extension", LabelSpec::One).referenceable(),
    BlockSpec::new("schema", LabelSpec::One).referenceable(),
    BlockSpec::new("table", LabelSpec::Qualified)
        .referenceable()
        .nested(TABLE),
    BlockSpec::new("view", LabelSpec::Qualified)
        .referenceable()
        .nested(VIEW),
    BlockSpec::new("materialized_view", LabelSpec::Qualified)
        .referenceable()
        .nested(MATERIALIZED_VIEW),
    BlockSpec::new("function", LabelSpec::Qualified)
        .referenceable()
        .nested(FUNCTION),
    BlockSpec::new("procedure", LabelSpec::Qualified)
        .referenceable()
        .nested(PROCEDURE),
    BlockSpec::new("trigger", LabelSpec::One),
    BlockSpec::new("policy", LabelSpec::One),
    BlockSpec::new("event_trigger", LabelSpec::One),
    BlockSpec::new("enum", LabelSpec::Qualified).referenceable(),
    BlockSpec::new("composite", LabelSpec::Qualified)
        .referenceable()
        .nested(COMPOSITE),
    BlockSpec::new("domain", LabelSpec::Qualified)
        .referenceable()
        .nested(DOMAIN),
    BlockSpec::new("sequence", LabelSpec::Qualified).referenceable(),
    BlockSpec::new("role", LabelSpec::One).referenceable(),
    BlockSpec::new("grant", LabelSpec::None),
    BlockSpec::new("revoke", LabelSpec::None),
    BlockSpec::new("data", LabelSpec::Qualified)
        .referenceable()
        .mergeable(),
];

pub fn schema_spec(settings: &PgSettings) -> SchemaSpec {
    SchemaSpec {
        blocks: BLOCKS,
        default_namespace: settings.default_schema.clone(),
    }
}
