use std::fmt;

/// `${VAR}` was referenced but not set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error("environment variable `{0}` is not set")]
    Missing(String),
    #[error("unterminated `${{` in `{0}`")]
    Unterminated(String),
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

/// Expand `${VAR}`, `${VAR:-default}` and `$$` using `lookup`.
pub fn expand(input: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String, EnvError> {
    let _ = (input, &lookup);
    unimplemented!("env expansion")
}
