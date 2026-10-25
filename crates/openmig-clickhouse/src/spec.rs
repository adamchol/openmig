//! The ClickHouse block vocabulary (see `docs/spec/hcl.md`).

use crate::settings::ChSettings;
use openmig_hcl::{Address, BlockSpec, LabelSpec, SchemaSpec};

const TABLE: &[BlockSpec] = &[
    BlockSpec::new("column", LabelSpec::One).referenceable(),
    BlockSpec::new("index", LabelSpec::One).referenceable(),
    BlockSpec::new("projection", LabelSpec::One).referenceable(),
    BlockSpec::new("constraint", LabelSpec::One).referenceable(),
];

const DICTIONARY: &[BlockSpec] = &[BlockSpec::new("attribute", LabelSpec::One).referenceable()];

const BLOCKS: &[BlockSpec] = &[
    BlockSpec::new("database", LabelSpec::One).referenceable(),
    BlockSpec::new("table", LabelSpec::Qualified)
        .referenceable()
        .nested(TABLE),
    BlockSpec::new("view", LabelSpec::Qualified).referenceable(),
    BlockSpec::new("materialized_view", LabelSpec::Qualified).referenceable(),
    BlockSpec::new("dictionary", LabelSpec::Qualified)
        .referenceable()
        .nested(DICTIONARY),
    BlockSpec::new("function", LabelSpec::One).referenceable(),
    BlockSpec::new("role", LabelSpec::One).referenceable(),
    BlockSpec::new("grant", LabelSpec::None),
    BlockSpec::new("row_policy", LabelSpec::One),
    BlockSpec::new("data", LabelSpec::Qualified)
        .referenceable()
        .mergeable(),
];

/// The vocabulary; the `default` database (and the configured default database) can be
/// referenced without a `database` block.
pub fn schema_spec(settings: &ChSettings) -> SchemaSpec {
    let mut implicit = vec![Address::new("database", vec!["default".to_owned()])];
    if settings.default_database != "default" {
        implicit.push(Address::new(
            "database",
            vec![settings.default_database.clone()],
        ));
    }
    SchemaSpec {
        blocks: BLOCKS,
        default_namespace: settings.default_database.clone(),
        implicit,
    }
}
