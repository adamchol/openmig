use std::fmt;

/// `${VAR}` was referenced but not set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error("environment variable `{0}` is not set")]
    Missing(String),
    #[error("unterminated `${{` in `{0}`")]
    Unterminated(String),
    #[error("invalid variable name `{name}` in `{input}`")]
    InvalidName { name: String, input: String },
}

/// A config string that may contain `${VAR}`, `${VAR:-default}` or `$$` (a literal `$`).
/// Expansion happens only when the value is needed, so an unset variable is an error only for
/// commands that use it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Expand(pub String);

impl Expand {
    pub fn raw(&self) -> &str {
        &self.0
    }

    pub fn resolve(&self) -> Result<String, EnvError> {
        expand(&self.0, |k| std::env::var(k).ok())
    }
}

impl fmt::Debug for Expand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Expand {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Expand)
    }
}

/// Expand `${VAR}`, `${VAR:-default}` and `$$` using `lookup`. A `$` not followed by `{` or `$`
/// is kept as is. An empty variable counts as unset for `:-`.
pub fn expand(input: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String, EnvError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(body) = after.strip_prefix('{') {
            let end = body
                .find('}')
                .ok_or_else(|| EnvError::Unterminated(input.to_owned()))?;
            let inner = &body[..end];
            let (name, default) = match inner.split_once(":-") {
                Some((n, d)) => (n, Some(d)),
                None => (inner, None),
            };
            let valid = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            if !valid {
                return Err(EnvError::InvalidName {
                    name: name.to_owned(),
                    input: input.to_owned(),
                });
            }
            match (
                lookup(name).filter(|v| !v.is_empty() || default.is_none()),
                default,
            ) {
                (Some(v), _) => out.push_str(&v),
                (None, Some(d)) => out.push_str(d),
                (None, None) => return Err(EnvError::Missing(name.to_owned())),
            }
            rest = &body[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(k: &str) -> Option<String> {
        match k {
            "HOST" => Some("db".into()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    #[test]
    fn expands() {
        assert_eq!(
            expand("postgres://${HOST}/x", env).unwrap(),
            "postgres://db/x"
        );
        assert_eq!(expand("${MISSING:-local}", env).unwrap(), "local");
        assert_eq!(expand("${EMPTY:-fallback}", env).unwrap(), "fallback");
        assert_eq!(expand("${EMPTY}", env).unwrap(), "");
        assert_eq!(expand("a$$b$c", env).unwrap(), "a$b$c");
        assert_eq!(
            expand("${MISSING}", env).unwrap_err(),
            EnvError::Missing("MISSING".into())
        );
        assert!(matches!(
            expand("${HOST", env).unwrap_err(),
            EnvError::Unterminated(_)
        ));
        assert!(matches!(
            expand("${1X}", env).unwrap_err(),
            EnvError::InvalidName { .. }
        ));
    }
}
