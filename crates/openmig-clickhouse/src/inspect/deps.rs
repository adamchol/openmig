//! Dependencies the server reports: `system.tables.dependencies_*` (materialized view sources)
//! and `loading_dependencies_*` (dictionaries and tables using them), plus the edges that follow
//! from definitions ([`crate::deps::definition_edges`]).

use super::Snapshot;
use crate::deps::{definition_edges, relation};
use crate::model::{Dependency, Model};
use openmig_core::ObjectRef;

pub(super) fn assemble(model: &mut Model, snapshot: &Snapshot) {
    let mut edges = Vec::new();
    let mut hard = |from: ObjectRef, to: Option<ObjectRef>| {
        if let Some(to) = to
            && from != to
        {
            edges.push(Dependency {
                from,
                to,
                soft: false,
            });
        }
    };
    for row in &snapshot.tables {
        let Some(this) = relation(model, &row.database, &row.name) else {
            continue;
        };
        for (db, name) in row
            .dependencies_database
            .iter()
            .zip(&row.dependencies_table)
        {
            if let Some(dependant) = relation(model, db, name) {
                hard(dependant, Some(this.clone()));
            }
        }
        for (db, name) in row
            .loading_dependencies_database
            .iter()
            .zip(&row.loading_dependencies_table)
        {
            hard(this.clone(), relation(model, db, name));
        }
    }
    edges.extend(definition_edges(model));
    model.dependencies = edges;
}
