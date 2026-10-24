use super::{Acl, ColumnRef};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    /// Labels in sort order.
    pub values: Vec<String>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composite {
    pub name: String,
    pub fields: Vec<CompositeField>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompositeField {
    pub name: String,
    pub data_type: String,
    /// Collation when it differs from the type's default.
    pub collation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Domain {
    pub name: String,
    /// Base type with modifiers: `numeric(10,2)`.
    pub data_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    /// Collation when it differs from the base type's default.
    pub collation: Option<String>,
    pub checks: BTreeMap<String, DomainCheck>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainCheck {
    pub name: String,
    /// The condition without `CHECK (...)`, referring to the value as `VALUE`.
    pub expr: String,
    pub validated: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sequence {
    pub name: String,
    /// `smallint`, `integer` or `bigint`.
    pub data_type: String,
    pub options: SequenceOptions,
    /// `OWNED BY` column.
    pub owned_by: Option<ColumnRef>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

impl Default for Sequence {
    /// A `bigint` sequence with PostgreSQL's default parameters.
    fn default() -> Self {
        Self {
            name: String::new(),
            data_type: "bigint".into(),
            options: SequenceOptions::default(),
            owned_by: None,
            comment: None,
            owner: String::new(),
            acl: Acl::new(),
        }
    }
}

/// Sequence parameters, always explicit. The current value is never part of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceOptions {
    pub start: i64,
    pub increment: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cache: i64,
    pub cycle: bool,
}

impl SequenceOptions {
    /// PostgreSQL's defaults for an ascending sequence of `data_type` (`smallint`, `integer`,
    /// anything else is treated as `bigint`).
    pub fn defaults_for(data_type: &str) -> Self {
        let max_value = match data_type {
            "smallint" => i64::from(i16::MAX),
            "integer" => i64::from(i32::MAX),
            _ => i64::MAX,
        };
        Self {
            start: 1,
            increment: 1,
            min_value: 1,
            max_value,
            cache: 1,
            cycle: false,
        }
    }
}

impl Default for SequenceOptions {
    fn default() -> Self {
        Self::defaults_for("bigint")
    }
}
