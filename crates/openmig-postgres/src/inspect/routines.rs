use super::{Catalog, SystemCatalog, acl, code, unexpected};
use crate::model::{
    ArgMode, Database, Function, FunctionArg, FunctionBody, Parallel, ReturnColumn, Returns,
    Volatility,
};
use openmig_core::{Error, ObjectKind, ObjectRef};
use std::collections::BTreeMap;
use tokio_postgres::Row;
use tokio_postgres::types::Json;

/// `[name, mode, type, default]` per argument.
type ArgRow = (Option<String>, String, String, Option<String>);

pub(super) fn functions(db: &mut Database, cat: &mut Catalog, rows: &[Row]) -> Result<(), Error> {
    for row in rows {
        let nsp: u32 = row.get("nsp");
        let Some(schema_name) = cat.schema(nsp).map(str::to_owned) else {
            continue;
        };
        let name: String = row.get("name");
        let kind = code(row, "kind");
        let mut args = Vec::new();
        let mut table_columns = Vec::new();
        let arg_rows = row
            .get::<_, Option<Json<Vec<ArgRow>>>>("args")
            .map(|Json(a)| a)
            .unwrap_or_default();
        for (arg_name, mode, data_type, default) in arg_rows {
            let arg_name = arg_name.filter(|n| !n.is_empty());
            let mode = match mode.as_str() {
                "i" => ArgMode::In,
                "o" => ArgMode::Out,
                "b" => ArgMode::InOut,
                "v" => ArgMode::Variadic,
                "t" => {
                    table_columns.push(ReturnColumn {
                        name: arg_name.unwrap_or_default(),
                        data_type,
                    });
                    continue;
                }
                other => return Err(unexpected("proargmode", other)),
            };
            args.push(FunctionArg {
                name: arg_name,
                mode,
                data_type,
                default,
            });
        }
        let returns = match kind {
            'p' => None,
            _ if !table_columns.is_empty() => Some(Returns::Table(table_columns)),
            _ => Some(Returns::Type {
                data_type: row.get("rettype"),
                setof: row.get("retset"),
            }),
        };
        let body = match row.get::<_, Option<String>>("sqlbody") {
            Some(body) => FunctionBody::Standard(body),
            None => FunctionBody::Definition(row.get("src")),
        };
        let config: Vec<String> = row.get("config");
        let config: BTreeMap<String, String> = config
            .into_iter()
            .map(|c| match c.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (c, String::new()),
            })
            .collect();
        let function = Function {
            name,
            args,
            returns,
            language: row.get("language"),
            body,
            volatility: match code(row, "volatility") {
                'v' => Volatility::Volatile,
                's' => Volatility::Stable,
                'i' => Volatility::Immutable,
                other => return Err(unexpected("provolatile", other)),
            },
            strict: row.get("strict"),
            security_definer: row.get("security_definer"),
            leakproof: row.get("leakproof"),
            parallel: match code(row, "parallel") {
                'u' => Parallel::Unsafe,
                'r' => Parallel::Restricted,
                's' => Parallel::Safe,
                other => return Err(unexpected("proparallel", other)),
            },
            cost: row.get("cost"),
            rows: row.get("rows"),
            window: kind == 'w',
            config,
            comment: row.get("comment"),
            owner: row.get("owner"),
            acl: acl(row, "acl")?,
        };
        let signature = function.signature();
        let schema = db
            .schemas
            .get_mut(&schema_name)
            .expect("functions belong to inspected schemas");
        let object_kind = if kind == 'p' {
            schema.procedures.insert(signature.clone(), function);
            ObjectKind::Procedure
        } else {
            schema.functions.insert(signature.clone(), function);
            ObjectKind::Function
        };
        cat.objects.insert(
            (SystemCatalog::Proc, row.get("oid")),
            ObjectRef::new(object_kind, [schema_name, signature]),
        );
    }
    Ok(())
}
