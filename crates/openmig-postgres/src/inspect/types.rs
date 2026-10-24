use super::{Catalog, RelKind, Relation, SystemCatalog, acl, code, unexpected};
use crate::model::{ColumnRef, Composite, Database, Domain, Enum, Sequence, SequenceOptions};
use openmig_core::{Error, ObjectKind, ObjectRef};
use std::collections::BTreeMap;
use tokio_postgres::Row;

/// Enums, domains and composite types.
pub(super) fn types(db: &mut Database, cat: &mut Catalog, rows: &[Row]) -> Result<(), Error> {
    for row in rows {
        let nsp: u32 = row.get("nsp");
        let Some(schema_name) = cat.schema(nsp).map(str::to_owned) else {
            continue;
        };
        let schema = db
            .schemas
            .get_mut(&schema_name)
            .expect("schema of a known namespace");
        let oid: u32 = row.get("oid");
        let name: String = row.get("name");
        let owner: String = row.get("owner");
        let comment: Option<String> = row.get("comment");
        let acl = acl(row, "acl")?;
        let kind = match code(row, "type") {
            'e' => {
                schema.enums.insert(
                    name.clone(),
                    Enum {
                        name: name.clone(),
                        values: row.get("labels"),
                        comment,
                        owner,
                        acl,
                    },
                );
                ObjectKind::Enum
            }
            'd' => {
                schema.domains.insert(
                    name.clone(),
                    Domain {
                        name: name.clone(),
                        data_type: row.get("base"),
                        not_null: row.get("not_null"),
                        default: row.get("default"),
                        collation: row.get("collation"),
                        checks: BTreeMap::new(),
                        comment,
                        owner,
                        acl,
                    },
                );
                ObjectKind::Domain
            }
            'c' => {
                schema.composites.insert(
                    name.clone(),
                    Composite {
                        name: name.clone(),
                        fields: Vec::new(),
                        comment,
                        owner,
                        acl,
                    },
                );
                cat.relations.insert(
                    row.get("rel"),
                    Relation {
                        schema: schema_name.clone(),
                        name: name.clone(),
                        kind: RelKind::Composite,
                    },
                );
                ObjectKind::Composite
            }
            other => return Err(unexpected("typtype", other)),
        };
        cat.objects.insert(
            (SystemCatalog::Type, oid),
            ObjectRef::new(kind, [schema_name, name]),
        );
    }
    Ok(())
}

/// Standalone sequences go into the model; identity sequences are returned by owning column.
pub(super) fn sequences(
    db: &mut Database,
    cat: &mut Catalog,
    rows: &[Row],
) -> Result<BTreeMap<(u32, i16), SequenceOptions>, Error> {
    let mut identities = BTreeMap::new();
    for row in rows {
        let nsp: u32 = row.get("nsp");
        let Some(schema_name) = cat.schema(nsp).map(str::to_owned) else {
            continue;
        };
        let oid: u32 = row.get("oid");
        let name: String = row.get("name");
        let options = SequenceOptions {
            start: row.get("start"),
            increment: row.get("increment"),
            min_value: row.get("min"),
            max_value: row.get("max"),
            cache: row.get("cache"),
            cycle: row.get("cycle"),
        };
        let deptype: Option<String> = row.get("deptype");
        if deptype.as_deref() == Some("i") {
            identities.insert((row.get("owner_rel"), row.get("owner_col")), options);
            cat.relations.insert(
                oid,
                Relation {
                    schema: schema_name,
                    name,
                    kind: RelKind::IdentitySequence,
                },
            );
            continue;
        }
        let owned_by = match (
            deptype.as_deref(),
            row.get::<_, Option<String>>("owner_schema"),
            row.get::<_, Option<String>>("owner_table"),
            row.get::<_, Option<String>>("owner_column"),
        ) {
            (Some("a"), Some(schema), Some(table), Some(column)) => Some(ColumnRef {
                schema,
                table,
                column,
            }),
            _ => None,
        };
        let schema = db
            .schemas
            .get_mut(&schema_name)
            .expect("schema of a known namespace");
        schema.sequences.insert(
            name.clone(),
            Sequence {
                name: name.clone(),
                data_type: row.get("type"),
                options,
                owned_by,
                comment: row.get("comment"),
                owner: row.get("owner"),
                acl: acl(row, "acl")?,
            },
        );
        cat.relations.insert(
            oid,
            Relation {
                schema: schema_name,
                name,
                kind: RelKind::Sequence,
            },
        );
    }
    Ok(identities)
}
