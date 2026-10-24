use indexmap::IndexMap;
use std::fmt;

/// A number as written in HCL. The original text is kept so decimals survive round trips.
#[derive(Clone, Debug, PartialEq)]
pub enum Number {
    Int(i64),
    Float(f64),
    /// Integers or decimals that do not fit `i64`/`f64` exactly, kept verbatim.
    Text(String),
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Number::Int(i) => write!(f, "{i}"),
            Number::Float(x) => write!(f, "{x}"),
            Number::Text(s) => f.write_str(s),
        }
    }
}

/// Identity of a referenceable block, e.g. `table.app.users` or `table.app.users.column.id`.
///
/// `labels` are canonical: qualified kinds always carry their namespace (the default namespace is
/// filled in when the label omits it).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub kind: String,
    pub labels: Vec<String>,
    /// Nested blocks below the top-level block, outermost first: `[("column", "id")]`.
    pub nested: Vec<(String, String)>,
}

impl Address {
    pub fn new(kind: impl Into<String>, labels: Vec<String>) -> Self {
        Self {
            kind: kind.into(),
            labels,
            nested: Vec::new(),
        }
    }

    pub fn child(&self, kind: impl Into<String>, label: impl Into<String>) -> Self {
        let mut a = self.clone();
        a.nested.push((kind.into(), label.into()));
        a
    }

    /// The top-level block this address belongs to.
    pub fn root(&self) -> Address {
        Address::new(self.kind.clone(), self.labels.clone())
    }

    /// Kind of the innermost block.
    pub fn leaf_kind(&self) -> &str {
        self.nested.last().map_or(&self.kind, |(k, _)| k)
    }

    /// Label of the innermost block (last label of the top-level block if not nested).
    pub fn leaf_label(&self) -> &str {
        self.nested
            .last()
            .map(|(_, l)| l.as_str())
            .or_else(|| self.labels.last().map(String::as_str))
            .unwrap_or("")
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind)?;
        for l in &self.labels {
            write_segment(f, l)?;
        }
        for (k, l) in &self.nested {
            write!(f, ".{k}")?;
            write_segment(f, l)?;
        }
        Ok(())
    }
}

fn write_segment(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !s.starts_with(|c: char| c.is_ascii_digit() || c == '-');
    if plain {
        write!(f, ".{s}")
    } else {
        write!(f, "[{s:?}]")
    }
}

/// The result of evaluating an HCL expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    List(Vec<Value>),
    Object(IndexMap<String, Value>),
    /// A reference to a referenceable block.
    Ref(Address),
    /// A bare identifier that is not a variable or block kind: `text`, `CASCADE`, `PUBLIC`.
    Ident(String),
    /// A call to a function that is not an HCL function: `varchar(255)`, `ZSTD(1)`,
    /// `ReplicatedMergeTree("/path", "{replica}")`. Engines render these to SQL.
    Call {
        name: String,
        args: Vec<Value>,
    },
    /// Raw SQL produced by `sql("...")`.
    Sql(String),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Object(_) => "object",
            Value::Ref(_) => "reference",
            Value::Ident(_) => "identifier",
            Value::Call { .. } => "call",
            Value::Sql(_) => "sql",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}
