//! The server's privilege hierarchy (`system.privileges`) and canonical privilege sets.
//!
//! ClickHouse reports grants (`system.grants`) partly expanded: `GRANT ALL ON db.*` comes back as
//! the list of every privilege that can be granted on a database, because groups such as `ALL`
//! or `CREATE` also contain privileges that only exist globally. [`PrivilegeCatalog::collapse`]
//! turns any set of privileges granted on one object into a canonical minimal set: a group
//! replaces its members when every member that can be granted at that level is present,
//! recursively up to `ALL`. Applying it to both the declared and the inspected side makes
//! `GRANT ALL ON db.*` compare equal to the server's expanded report.

use crate::model::GrantLevel;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// One row of `system.privileges`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PrivilegeRow {
    pub privilege: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// `GLOBAL`, `DATABASE`, `TABLE`, `VIEW`, `DICTIONARY`, `COLUMN`, `NAMED_COLLECTION`,
    /// `USER_NAME`, `TABLE_ENGINE`, `DEFINER`, `SOURCE`; `None` for groups.
    pub level: Option<String>,
    pub parent_group: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Node {
    level: Option<String>,
    children: Vec<String>,
}

/// The privilege hierarchy of one server version.
#[derive(Clone, Debug, Default)]
pub struct PrivilegeCatalog {
    nodes: BTreeMap<String, Node>,
    /// Upper-cased names and aliases to canonical names.
    names: BTreeMap<String, String>,
    roots: Vec<String>,
}

const NONE: &str = "NONE";

impl PrivilegeCatalog {
    pub fn new(rows: impl IntoIterator<Item = PrivilegeRow>) -> Self {
        let rows: Vec<PrivilegeRow> = rows.into_iter().collect();
        let mut catalog = PrivilegeCatalog::default();
        for row in &rows {
            catalog.nodes.insert(
                row.privilege.clone(),
                Node {
                    level: row.level.clone(),
                    children: Vec::new(),
                },
            );
            for name in std::iter::once(&row.privilege).chain(&row.aliases) {
                catalog
                    .names
                    .insert(name.to_uppercase(), row.privilege.clone());
            }
        }
        for row in &rows {
            match row
                .parent_group
                .as_ref()
                .filter(|p| catalog.nodes.contains_key(*p))
            {
                Some(parent) => {
                    if let Some(node) = catalog.nodes.get_mut(parent) {
                        node.children.push(row.privilege.clone());
                    }
                }
                None => catalog.roots.push(row.privilege.clone()),
            }
        }
        for node in catalog.nodes.values_mut() {
            node.children.sort();
        }
        catalog.roots.sort();
        catalog
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The canonical name of a privilege or alias, matched case-insensitively
    /// (`update` is `ALTER UPDATE`, `ALL PRIVILEGES` is `ALL`).
    pub fn canonical(&self, name: &str) -> Option<&str> {
        self.names
            .get(&name.trim().to_uppercase())
            .map(String::as_str)
    }

    /// The canonical minimal form of `privileges` granted at `level`. Unknown names are kept as
    /// written; `NONE` is dropped. Privileges on parameters (named collections, sources, ...)
    /// are only canonicalized.
    pub fn collapse<S: AsRef<str>>(
        &self,
        privileges: impl IntoIterator<Item = S>,
        level: GrantLevel,
    ) -> BTreeSet<String> {
        let (canonical, mut out) = self.split_known(privileges);
        if level == GrantLevel::Parameter {
            out.extend(canonical.into_iter().map(str::to_owned));
            return out;
        }
        let (leaves, extra) = self.leaves(&canonical, level);
        out.extend(extra);
        for root in &self.roots {
            self.emit(root, &leaves, level, &mut out);
        }
        out
    }

    /// Every individual privilege that `privileges` grant at `level` (the inverse of
    /// [`PrivilegeCatalog::collapse`]). Unknown names are kept as written.
    pub fn expand<S: AsRef<str>>(
        &self,
        privileges: impl IntoIterator<Item = S>,
        level: GrantLevel,
    ) -> BTreeSet<String> {
        let (canonical, mut out) = self.split_known(privileges);
        if level == GrantLevel::Parameter {
            out.extend(canonical.into_iter().map(str::to_owned));
            return out;
        }
        let (leaves, extra) = self.leaves(&canonical, level);
        out.extend(extra);
        out.extend(leaves.into_iter().map(str::to_owned));
        out
    }

    /// Canonical names of known privileges, and unknown names as written.
    fn split_known<S: AsRef<str>>(
        &self,
        privileges: impl IntoIterator<Item = S>,
    ) -> (BTreeSet<&str>, BTreeSet<String>) {
        let mut known = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        for p in privileges {
            let p = p.as_ref();
            match self.canonical(p) {
                Some(NONE) => {}
                Some(name) => {
                    known.insert(name);
                }
                None => {
                    unknown.insert(p.trim().to_owned());
                }
            }
        }
        (known, unknown)
    }

    /// Privileges with their own flag grantable at `level` that `privileges` imply, and the
    /// given privileges that imply none (kept as written).
    fn leaves<'a>(
        &'a self,
        privileges: &BTreeSet<&'a str>,
        level: GrantLevel,
    ) -> (BTreeSet<&'a str>, BTreeSet<String>) {
        let mut leaves = BTreeSet::new();
        let mut extra = BTreeSet::new();
        for &p in privileges {
            if self.relevant(p, level) {
                self.collect(p, level, &mut leaves);
            } else {
                extra.insert(p.to_owned());
            }
        }
        (leaves, extra)
    }

    fn collect<'a>(&'a self, name: &'a str, level: GrantLevel, out: &mut BTreeSet<&'a str>) {
        let Some(node) = self.nodes.get(name) else {
            return;
        };
        if grantable(node.level.as_deref(), level) {
            out.insert(name);
        }
        for child in &node.children {
            self.collect(child, level, out);
        }
    }

    /// Whether `name` or one of its descendants can be granted at `level`.
    fn relevant(&self, name: &str, level: GrantLevel) -> bool {
        self.nodes.get(name).is_some_and(|node| {
            grantable(node.level.as_deref(), level)
                || node.children.iter().any(|c| self.relevant(c, level))
        })
    }

    /// Whether every privilege under `name` that can be granted at `level` is in `leaves`.
    fn full(&self, name: &str, leaves: &BTreeSet<&str>, level: GrantLevel) -> bool {
        let Some(node) = self.nodes.get(name) else {
            return false;
        };
        (!grantable(node.level.as_deref(), level) || leaves.contains(name))
            && node
                .children
                .iter()
                .filter(|c| self.relevant(c, level))
                .all(|c| self.full(c, leaves, level))
    }

    fn emit(
        &self,
        name: &str,
        leaves: &BTreeSet<&str>,
        level: GrantLevel,
        out: &mut BTreeSet<String>,
    ) {
        if !self.relevant(name, level) {
            return;
        }
        if self.full(name, leaves, level) {
            out.insert(name.to_owned());
            return;
        }
        let Some(node) = self.nodes.get(name) else {
            return;
        };
        // A privilege with its own flag and children whose children are only partly granted
        // cannot be written exactly; its name is the closest form.
        if grantable(node.level.as_deref(), level) && leaves.contains(name) {
            out.insert(name.to_owned());
        }
        for child in &node.children {
            self.emit(child, leaves, level, out);
        }
    }
}

/// Whether a privilege of `privilege_level` (`None` for groups, which have no flag of their
/// own) can be granted on an object of `level`.
fn grantable(privilege_level: Option<&str>, level: GrantLevel) -> bool {
    let Some(privilege_level) = privilege_level else {
        return false;
    };
    match level {
        GrantLevel::Global => true,
        GrantLevel::Database => matches!(
            privilege_level,
            "DATABASE" | "TABLE" | "VIEW" | "DICTIONARY" | "COLUMN"
        ),
        GrantLevel::Table => matches!(privilege_level, "TABLE" | "VIEW" | "DICTIONARY" | "COLUMN"),
        GrantLevel::Column => privilege_level == "COLUMN",
        GrantLevel::Parameter => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;

    const PRIVILEGES: &str = include_str!("../tests/fixtures/privileges-26.5.jsonl");
    /// `system.grants` of a role after `GRANT ALL ON *.*` and
    /// `GRANT ALL ON app.* WITH GRANT OPTION` on a server with `enable_read_write_grants`.
    const GRANTS_ALL: &str = include_str!("../tests/fixtures/grants-all-26.5.jsonl");

    #[derive(Deserialize)]
    struct GrantRow {
        access_type: String,
        database: Option<String>,
    }

    fn catalog() -> PrivilegeCatalog {
        PrivilegeCatalog::new(
            PRIVILEGES
                .lines()
                .map(|line| serde_json::from_str::<PrivilegeRow>(line).unwrap()),
        )
    }

    fn reported(database: Option<&str>) -> Vec<String> {
        GRANTS_ALL
            .lines()
            .map(|line| serde_json::from_str::<GrantRow>(line).unwrap())
            .filter(|row| row.database.as_deref() == database)
            .map(|row| row.access_type)
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn expanded_all_collapses_to_all() {
        let catalog = catalog();
        let global = reported(None);
        assert!(global.len() > 50, "{global:?}");
        assert_eq!(catalog.collapse(&global, GrantLevel::Global), set(&["ALL"]));
        let database = reported(Some("app"));
        assert!(database.contains(&"CREATE TABLE".to_owned()));
        assert!(!database.contains(&"ALL".to_owned()));
        assert_eq!(
            catalog.collapse(&database, GrantLevel::Database),
            set(&["ALL"])
        );
    }

    #[test]
    fn expand_is_the_inverse_of_collapse() {
        let catalog = catalog();
        let database = reported(Some("app"));
        let expanded = catalog.expand(["ALL"], GrantLevel::Database);
        assert_eq!(catalog.expand(&database, GrantLevel::Database), expanded);
        assert!(expanded.contains("SELECT"));
        assert!(expanded.contains("ALTER UPDATE"));
        assert!(!expanded.contains("CREATE USER"));
        assert!(!expanded.contains("ALTER"));
        assert_eq!(
            catalog.collapse(&expanded, GrantLevel::Database),
            set(&["ALL"])
        );
        let global = catalog.expand(["ALL"], GrantLevel::Global);
        assert!(global.contains("CREATE USER"));
        assert!(global.len() > expanded.len());
    }

    #[test]
    fn groups_collapse_per_level() {
        let catalog = catalog();
        assert_eq!(
            catalog.collapse(
                [
                    "CREATE DATABASE",
                    "CREATE TABLE",
                    "CREATE VIEW",
                    "CREATE DICTIONARY"
                ],
                GrantLevel::Database
            ),
            set(&["CREATE"])
        );
        assert_eq!(
            catalog.collapse(
                [
                    "CREATE DATABASE",
                    "CREATE TABLE",
                    "CREATE VIEW",
                    "CREATE DICTIONARY"
                ],
                GrantLevel::Global
            ),
            set(&[
                "CREATE DATABASE",
                "CREATE DICTIONARY",
                "CREATE TABLE",
                "CREATE VIEW"
            ])
        );
        assert_eq!(
            catalog.collapse(["CREATE"], GrantLevel::Table),
            catalog.collapse(
                ["CREATE TABLE", "CREATE VIEW", "CREATE DICTIONARY"],
                GrantLevel::Table
            )
        );
        assert_eq!(
            catalog.collapse(["SELECT", "INSERT"], GrantLevel::Database),
            set(&["INSERT", "SELECT"])
        );
        assert_eq!(
            catalog.collapse(["SELECT", "INSERT"], GrantLevel::Column),
            set(&["INSERT", "SELECT"])
        );
        let column_all = catalog.expand(["ALL"], GrantLevel::Column);
        assert!(column_all.contains("SELECT") && column_all.contains("ALTER UPDATE"));
        assert!(!column_all.contains("CHECK"));
        assert_eq!(
            catalog.collapse(&column_all, GrantLevel::Column),
            set(&["ALL"])
        );
    }

    #[test]
    fn names_and_aliases() {
        let catalog = catalog();
        assert_eq!(catalog.canonical("update"), Some("ALTER UPDATE"));
        assert_eq!(catalog.canonical("ALL PRIVILEGES"), Some("ALL"));
        assert_eq!(catalog.canonical("DICTGET"), Some("dictGet"));
        assert_eq!(
            catalog.collapse(["select", "UPDATE", "usage"], GrantLevel::Table),
            set(&["ALTER UPDATE", "SELECT"])
        );
        assert_eq!(
            catalog.collapse(["SELECT", "FUTURE PRIVILEGE"], GrantLevel::Table),
            set(&["FUTURE PRIVILEGE", "SELECT"])
        );
        assert_eq!(
            catalog.collapse(["NAMED COLLECTION ADMIN"], GrantLevel::Parameter),
            set(&["NAMED COLLECTION ADMIN"])
        );
        assert_eq!(
            catalog.collapse(["READ"], GrantLevel::Parameter),
            set(&["READ"])
        );
    }

    #[test]
    fn privileges_not_grantable_at_a_level_are_kept() {
        let catalog = catalog();
        assert_eq!(
            catalog.collapse(["CREATE USER"], GrantLevel::Database),
            set(&["CREATE USER"])
        );
    }
}
