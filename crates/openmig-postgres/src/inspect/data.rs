use crate::conn::describe;
use crate::model::{Database, QualifiedName, TableData};
use openmig_core::sql::{Dialect, quote_ident};
use openmig_core::{Error, ObjectKind, ObjectRef, Scope};
use std::collections::BTreeMap;
use tokio_postgres::Client;

/// Rows of the data-managed tables that exist, in text form.
pub(super) async fn load(client: &Client, db: &mut Database, scope: &Scope) -> Result<(), Error> {
    for managed in &scope.data_tables {
        let [schema, table] = managed.path.as_slice() else {
            continue;
        };
        let name = QualifiedName::new(schema, table);
        if scope
            .exclude
            .is_excluded(&ObjectRef::new(ObjectKind::Table, [schema, table]))
        {
            continue;
        }
        let Some(t) = db.table(&name) else {
            continue;
        };
        let all: Vec<String> = t
            .columns
            .iter()
            .filter(|c| c.generated.is_none())
            .map(|c| c.name.clone())
            .collect();
        let key = match &t.primary_key {
            Some(pk) => pk.columns.clone(),
            None => all.clone(),
        };
        let columns: Vec<String> = if managed.columns.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|c| key.contains(c) || managed.columns.contains(c))
                .collect()
        };
        let fail = |e: tokio_postgres::Error| {
            Error::Inspect(format!("reading rows of {name}: {}", describe(&e)))
        };
        let max = scope.data_max_rows;
        let count: i64 = client
            .query_one(
                &format!(
                    "SELECT count(*) FROM (SELECT 1 FROM {} LIMIT {}) s",
                    name.to_sql(),
                    max.saturating_add(1)
                ),
                &[],
            )
            .await
            .map_err(fail)?
            .get(0);
        if u64::try_from(count).unwrap_or_default() > max {
            return Err(Error::Inspect(format!(
                "table {name} has more than {max} rows (data_max_rows); refusing to manage its data"
            )));
        }
        let select = format!(
            "SELECT {} FROM {}",
            columns
                .iter()
                .map(|c| format!("{}::text", quote_ident(c, Dialect::Postgres)))
                .collect::<Vec<_>>()
                .join(", "),
            name.to_sql()
        );
        let rows = client.query(&select, &[]).await.map_err(fail)?;
        let mut data = TableData {
            key,
            rows: BTreeMap::new(),
            mode: None,
        };
        for row in rows {
            let values: BTreeMap<String, Option<String>> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| (c.clone(), row.get(i)))
                .collect();
            let key: Vec<Option<String>> = data
                .key
                .iter()
                .map(|c| values.get(c).cloned().flatten())
                .collect();
            if data.rows.insert(key.clone(), values).is_some() {
                return Err(Error::Inspect(format!(
                    "table {name} has several rows with the key {key:?}; data blocks need a unique key"
                )));
            }
        }
        db.data.insert(name, data);
    }
    Ok(())
}
