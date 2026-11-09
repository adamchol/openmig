use super::*;
use crate::model::{Composite, Domain, Enum, Schema};

fn db() -> Database {
    let mut s = Schema::new("app");
    s.enums.insert(
        "status".into(),
        Enum {
            name: "status".into(),
            ..Enum::default()
        },
    );
    s.enums.insert(
        "mood".into(),
        Enum {
            name: "mood".into(),
            ..Enum::default()
        },
    );
    s.composites.insert(
        "pair".into(),
        Composite {
            name: "pair".into(),
            ..Composite::default()
        },
    );
    s.domains.insert(
        "email".into(),
        Domain {
            name: "email".into(),
            data_type: "text".into(),
            ..Domain::default()
        },
    );
    s.domains.insert(
        "amount".into(),
        Domain {
            name: "amount".into(),
            data_type: "numeric(12,2)".into(),
            ..Domain::default()
        },
    );
    let mut db = Database::default();
    db.schemas.insert("app".into(), s);
    db
}

fn can(from: &str, to: &str) -> bool {
    let db = db();
    castable(&db, &db, 17, from, to)
}

#[test]
fn table_is_sorted() {
    assert!(BUILTIN.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn builtin_casts() {
    assert!(can("integer", "bigint"));
    assert!(can("bigint", "integer"));
    assert!(can("numeric(10,2)", "double precision"));
    assert!(can(
        "timestamp(3) without time zone",
        "timestamp with time zone"
    ));
    assert!(can("json", "jsonb"));
    assert!(!can("uuid", "integer"));
    assert!(!can("integer", "uuid"));
    assert!(!can("boolean", "date"));
    assert!(!can("jsonb", "timestamp with time zone"));
}

#[test]
fn modifiers_and_strings() {
    assert!(can("character varying(10)", "character varying(20)"));
    assert!(can("interval day to second", "interval(3)"));
    assert!(can("uuid", "text"));
    assert!(can("text", "uuid"));
    assert!(can("character varying(36)", "uuid"));
    assert!(can("app.status", "text"));
    assert!(can("text", "app.status"));
    assert!(can("integer[]", "text"));
}

#[test]
fn arrays() {
    assert!(can("integer[]", "bigint[]"));
    assert!(can("text[]", "app.status[]"));
    assert!(!can("uuid[]", "integer[]"));
    assert!(!can("integer[]", "integer"));
    assert!(!can("integer", "integer[]"));
}

#[test]
fn defined_types() {
    assert!(!can("app.status", "app.mood"));
    assert!(!can("app.status", "integer"));
    assert!(!can("app.pair", "jsonb"));
    assert!(can("app.amount", "integer"));
    assert!(can("bigint", "app.amount"));
    assert!(can("app.email", "app.status"));
    assert!(!can("uuid", "app.amount"));
}

#[test]
fn unknown_types_are_left_to_the_server() {
    assert!(can("public.citext", "integer"));
    assert!(can("uuid", "public.hstore"));
}

#[test]
fn versioned() {
    let db = db();
    assert!(!castable(&db, &db, 17, "bytea", "integer"));
    assert!(castable(&db, &db, 18, "bytea", "integer"));
    assert!(castable(&db, &db, 14, "path", "point"));
    assert!(!castable(&db, &db, 15, "path", "point"));
}
