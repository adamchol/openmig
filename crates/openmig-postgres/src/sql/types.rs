//! Enums, composite types, domains and sequences.

use super::{ident, literal, qualified, sequence_options};
use crate::diff::EnumPosition;
use crate::model::{ColumnRef, Composite, CompositeField, Domain, DomainCheck, Sequence};

pub(crate) fn create_enum(schema: &str, name: &str, values: &[String]) -> String {
    let values: Vec<String> = values.iter().map(|v| literal(v)).collect();
    format!(
        "CREATE TYPE {} AS ENUM ({})",
        qualified(schema, name),
        values.join(", ")
    )
}

pub(crate) fn add_enum_value(schema: &str, name: &str, value: &str, at: &EnumPosition) -> String {
    let position = match at {
        EnumPosition::Before(v) => format!(" BEFORE {}", literal(v)),
        EnumPosition::After(v) => format!(" AFTER {}", literal(v)),
        EnumPosition::End => String::new(),
    };
    format!(
        "ALTER TYPE {} ADD VALUE {}{position}",
        qualified(schema, name),
        literal(value)
    )
}

/// `DROP TYPE` for enums and composite types.
pub(crate) fn drop_type(schema: &str, name: &str) -> String {
    format!("DROP TYPE {}", qualified(schema, name))
}

fn field(f: &CompositeField) -> String {
    let collate = f
        .collation
        .as_ref()
        .map(|c| format!(" COLLATE {c}"))
        .unwrap_or_default();
    format!("{} {}{collate}", ident(&f.name), f.data_type)
}

pub(crate) fn create_composite(schema: &str, c: &Composite) -> String {
    let fields: Vec<String> = c.fields.iter().map(field).collect();
    format!(
        "CREATE TYPE {} AS ({})",
        qualified(schema, &c.name),
        fields.join(", ")
    )
}

pub(crate) fn add_attribute(schema: &str, name: &str, f: &CompositeField) -> String {
    format!(
        "ALTER TYPE {} ADD ATTRIBUTE {}",
        qualified(schema, name),
        field(f)
    )
}

pub(crate) fn drop_attribute(schema: &str, name: &str, attribute: &str) -> String {
    format!(
        "ALTER TYPE {} DROP ATTRIBUTE {}",
        qualified(schema, name),
        ident(attribute)
    )
}

pub(crate) fn alter_attribute_type(schema: &str, name: &str, f: &CompositeField) -> String {
    let collate = f
        .collation
        .as_ref()
        .map(|c| format!(" COLLATE {c}"))
        .unwrap_or_default();
    format!(
        "ALTER TYPE {} ALTER ATTRIBUTE {} TYPE {}{collate}",
        qualified(schema, name),
        ident(&f.name),
        f.data_type
    )
}

pub(crate) fn domain_check_definition(c: &DomainCheck) -> String {
    format!("CONSTRAINT {} CHECK ({})", ident(&c.name), c.expr)
}

/// `CREATE DOMAIN` with the checks that are valid (others are added `NOT VALID` afterwards).
pub(crate) fn create_domain(schema: &str, d: &Domain) -> String {
    let mut sql = format!(
        "CREATE DOMAIN {} AS {}",
        qualified(schema, &d.name),
        d.data_type
    );
    if let Some(c) = &d.collation {
        sql.push_str(&format!(" COLLATE {c}"));
    }
    if let Some(def) = &d.default {
        sql.push_str(&format!(" DEFAULT {def}"));
    }
    if d.not_null {
        sql.push_str(" NOT NULL");
    }
    for c in d.checks.values().filter(|c| c.validated) {
        sql.push(' ');
        sql.push_str(&domain_check_definition(c));
    }
    sql
}

pub(crate) fn alter_domain(schema: &str, name: &str, action: &str) -> String {
    format!("ALTER DOMAIN {} {action}", qualified(schema, name))
}

pub(crate) fn drop_domain(schema: &str, name: &str) -> String {
    format!("DROP DOMAIN {}", qualified(schema, name))
}

pub(crate) fn create_sequence(schema: &str, s: &Sequence) -> String {
    format!(
        "CREATE SEQUENCE {} AS {} {}",
        qualified(schema, &s.name),
        s.data_type,
        sequence_options(&s.options)
    )
}

/// `ALTER SEQUENCE` with every option, so that a type change does not adjust the bounds.
pub(crate) fn alter_sequence(schema: &str, s: &Sequence) -> String {
    format!(
        "ALTER SEQUENCE {} AS {} {}",
        qualified(schema, &s.name),
        s.data_type,
        sequence_options(&s.options)
    )
}

pub(crate) fn sequence_owned_by(schema: &str, name: &str, owner: Option<&ColumnRef>) -> String {
    let owner = match owner {
        Some(c) => format!("{}.{}", qualified(&c.schema, &c.table), ident(&c.column)),
        None => "NONE".to_owned(),
    };
    format!(
        "ALTER SEQUENCE {} OWNED BY {owner}",
        qualified(schema, name)
    )
}

pub(crate) fn drop_sequence(schema: &str, name: &str) -> String {
    format!("DROP SEQUENCE {}", qualified(schema, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SequenceOptions;
    use std::collections::BTreeMap;

    #[test]
    fn enums() {
        assert_eq!(
            create_enum("app", "status", &["a".into(), "it's".into()]),
            "CREATE TYPE \"app\".\"status\" AS ENUM ('a', 'it''s')"
        );
        assert_eq!(
            add_enum_value("app", "status", "b", &EnumPosition::After("a".into())),
            "ALTER TYPE \"app\".\"status\" ADD VALUE 'b' AFTER 'a'"
        );
    }

    #[test]
    fn composites_and_domains() {
        let c = Composite {
            name: "address".into(),
            fields: vec![
                CompositeField {
                    name: "street".into(),
                    data_type: "text".into(),
                    collation: None,
                },
                CompositeField {
                    name: "city".into(),
                    data_type: "text".into(),
                    collation: Some("\"C\"".into()),
                },
            ],
            ..Composite::default()
        };
        assert_eq!(
            create_composite("app", &c),
            "CREATE TYPE \"app\".\"address\" AS (\"street\" text, \"city\" text COLLATE \"C\")"
        );
        let mut checks = BTreeMap::new();
        checks.insert(
            "positive".to_owned(),
            DomainCheck {
                name: "positive".into(),
                expr: "VALUE >= 0".into(),
                validated: true,
                comment: None,
            },
        );
        let d = Domain {
            name: "amount".into(),
            data_type: "numeric(12,2)".into(),
            not_null: true,
            default: Some("0".into()),
            checks,
            ..Domain::default()
        };
        assert_eq!(
            create_domain("app", &d),
            "CREATE DOMAIN \"app\".\"amount\" AS numeric(12,2) DEFAULT 0 NOT NULL CONSTRAINT \
             \"positive\" CHECK (VALUE >= 0)"
        );
    }

    #[test]
    fn sequences() {
        let s = Sequence {
            name: "n".into(),
            data_type: "integer".into(),
            options: SequenceOptions::defaults_for("integer"),
            ..Sequence::default()
        };
        assert_eq!(
            create_sequence("app", &s),
            "CREATE SEQUENCE \"app\".\"n\" AS integer START WITH 1 INCREMENT BY 1 MINVALUE 1 \
             MAXVALUE 2147483647 CACHE 1 NO CYCLE"
        );
        let owner = ColumnRef {
            schema: "app".into(),
            table: "t".into(),
            column: "id".into(),
        };
        assert_eq!(
            sequence_owned_by("app", "n", Some(&owner)),
            "ALTER SEQUENCE \"app\".\"n\" OWNED BY \"app\".\"t\".\"id\""
        );
    }
}
