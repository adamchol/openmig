//! Rows of data-managed tables. Values are PostgreSQL's text form, written as string literals
//! that PostgreSQL casts to the column type.

use super::{ident, ident_list, literal};
use crate::model::QualifiedName;

fn value(v: Option<&str>) -> String {
    v.map_or_else(|| "NULL".to_owned(), literal)
}

fn condition(key: &[(&str, Option<&str>)]) -> String {
    key.iter()
        .map(|(c, v)| match v {
            Some(v) => format!("{} = {}", ident(c), literal(v)),
            None => format!("{} IS NULL", ident(c)),
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// One multi-row `INSERT`. `overriding` adds `OVERRIDING SYSTEM VALUE` for identity columns
/// declared `GENERATED ALWAYS`.
pub(crate) fn insert_rows(
    table: &QualifiedName,
    columns: &[String],
    rows: &[Vec<Option<&str>>],
    overriding: bool,
) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|r| {
            let values: Vec<String> = r.iter().map(|v| value(*v)).collect();
            format!("({})", values.join(", "))
        })
        .collect();
    format!(
        "INSERT INTO {} ({}){} VALUES {}",
        table.to_sql(),
        ident_list(columns),
        if overriding {
            " OVERRIDING SYSTEM VALUE"
        } else {
            ""
        },
        rows.join(", ")
    )
}

pub(crate) fn update_row(
    table: &QualifiedName,
    set: &[(&str, Option<&str>)],
    key: &[(&str, Option<&str>)],
) -> String {
    let set: Vec<String> = set
        .iter()
        .map(|(c, v)| format!("{} = {}", ident(c), value(*v)))
        .collect();
    format!(
        "UPDATE {} SET {} WHERE {}",
        table.to_sql(),
        set.join(", "),
        condition(key)
    )
}

pub(crate) fn delete_row(table: &QualifiedName, key: &[(&str, Option<&str>)]) -> String {
    format!("DELETE FROM {} WHERE {}", table.to_sql(), condition(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows() {
        let t = QualifiedName::new("app", "statuses");
        assert_eq!(
            insert_rows(
                &t,
                &["code".into(), "label".into()],
                &[vec![Some("a"), Some("it's")], vec![Some("d"), None]],
                false
            ),
            "INSERT INTO \"app\".\"statuses\" (\"code\", \"label\") VALUES ('a', 'it''s'), ('d', \
             NULL)"
        );
        assert_eq!(
            update_row(&t, &[("label", None)], &[("code", Some("a"))]),
            "UPDATE \"app\".\"statuses\" SET \"label\" = NULL WHERE \"code\" = 'a'"
        );
        assert_eq!(
            delete_row(&t, &[("code", Some("a")), ("x", None)]),
            "DELETE FROM \"app\".\"statuses\" WHERE \"code\" = 'a' AND \"x\" IS NULL"
        );
    }
}
