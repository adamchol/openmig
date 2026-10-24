use crate::object::{ObjectKind, ObjectRef};
use globset::{GlobBuilder, GlobMatcher};

/// Objects OpenMig must neither inspect nor touch.
///
/// Pattern syntax: `[kind:]glob`, where `glob` is matched against the object's dotted path
/// (`app.users`, `app.users.email`, `web`). `*` does not cross dots; `**` does. Without a kind the
/// pattern matches schemas/databases, tables, views and every other object whose path matches, and
/// excluding a schema or table excludes everything inside it.
///
/// Examples: `role:cnpg_*`, `user:*`, `schema:graphile_*`, `app.legacy_*`, `function:app.debug_*`.
#[derive(Clone, Debug, Default)]
pub struct ExcludeSet {
    patterns: Vec<Pattern>,
}

#[derive(Clone, Debug)]
struct Pattern {
    source: String,
    kind: Option<ObjectKind>,
    glob: GlobMatcher,
}

impl ExcludeSet {
    pub fn new<S: AsRef<str>>(patterns: impl IntoIterator<Item = S>) -> Result<Self, String> {
        let mut out = Vec::new();
        for p in patterns {
            let p = p.as_ref();
            let (kind, glob) = match p.split_once(':') {
                Some((k, g)) => {
                    let kind = ObjectKind::parse(k)
                        .ok_or_else(|| format!("unknown object kind `{k}` in exclude `{p}`"))?;
                    (Some(kind), g)
                }
                None => (None, p),
            };
            let glob = GlobBuilder::new(&glob.replace('.', "/"))
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid exclude pattern `{p}`: {e}"))?
                .compile_matcher();
            out.push(Pattern {
                source: p.to_owned(),
                kind,
                glob,
            });
        }
        Ok(Self { patterns: out })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn patterns(&self) -> impl Iterator<Item = &str> {
        self.patterns.iter().map(|p| p.source.as_str())
    }

    /// Whether the object, or any container of it, is excluded.
    pub fn is_excluded(&self, object: &ObjectRef) -> bool {
        self.matches(object.kind, &object.path)
    }

    /// Whether an object of `kind` at `path` is excluded. Containers are checked too: a path of
    /// `["app", "users", "email"]` is excluded by `app` and by `app.users`.
    pub fn matches(&self, kind: ObjectKind, path: &[String]) -> bool {
        let joined = path.join("/");
        self.patterns.iter().any(|p| {
            let kind_ok = match p.kind {
                None => true,
                Some(k) => k == kind,
            };
            if kind_ok && p.glob.is_match(&joined) {
                return true;
            }
            p.kind.is_none() && (1..path.len()).any(|n| p.glob.is_match(path[..n].join("/")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(p: &[&str]) -> ExcludeSet {
        ExcludeSet::new(p).unwrap()
    }

    #[test]
    fn kinds_and_containers() {
        let s = set(&["role:cnpg_*", "app.legacy_*"]);
        assert!(s.is_excluded(&ObjectRef::new(ObjectKind::Role, ["cnpg_pooler"])));
        assert!(!s.is_excluded(&ObjectRef::new(ObjectKind::User, ["cnpg_pooler"])));
        assert!(s.is_excluded(&ObjectRef::new(ObjectKind::Table, ["app", "legacy_users"])));
        assert!(s.is_excluded(&ObjectRef::new(
            ObjectKind::Column,
            ["app", "legacy_users", "id"]
        )));
        assert!(!s.is_excluded(&ObjectRef::new(ObjectKind::Table, ["app", "users"])));
    }
}
