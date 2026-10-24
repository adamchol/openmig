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

impl Number {
    /// Classify the text of a number literal (`42`, `-1.5`, `1e10`, `1.50`).
    ///
    /// Integer literals that fit `i64` become [`Number::Int`]; decimals whose shortest `f64`
    /// rendering is the text itself become [`Number::Float`]; every other valid literal is kept
    /// verbatim as [`Number::Text`]. Returns `None` when `text` is not a number literal.
    pub fn from_literal(text: &str) -> Option<Number> {
        let digits = text.strip_prefix('-').unwrap_or(text);
        let (mantissa, exponent) = match digits.find(['e', 'E']) {
            Some(i) => (&digits[..i], Some(&digits[i + 1..])),
            None => (digits, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (mantissa, None),
        };
        let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let exponent_ok =
            exponent.is_none_or(|e| all_digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
        if !all_digits(int_part) || !frac_part.is_none_or(all_digits) || !exponent_ok {
            return None;
        }
        if frac_part.is_none() && exponent.is_none() {
            return Some(match text.parse::<i64>() {
                Ok(i) => Number::Int(i),
                Err(_) => Number::Text(text.to_owned()),
            });
        }
        match text.parse::<f64>() {
            Ok(f) if f.is_finite() && f.to_string() == text => Some(Number::Float(f)),
            _ => Some(Number::Text(text.to_owned())),
        }
    }

    /// The number from an arithmetic result: integral values that fit `i64` become
    /// [`Number::Int`]. Returns `None` for NaN and infinities.
    pub fn from_f64(f: f64) -> Option<Number> {
        if !f.is_finite() {
            return None;
        }
        // The range check keeps `as` from saturating: 2^63 is exactly representable.
        if f.fract() == 0.0 && f >= -(2f64.powi(63)) && f < 2f64.powi(63) {
            Some(Number::Int(f as i64))
        } else {
            Some(Number::Float(f))
        }
    }

    /// The value as `f64` (approximate for large integers and long decimals).
    pub fn as_f64(&self) -> f64 {
        match self {
            Number::Int(i) => *i as f64,
            Number::Float(f) => *f,
            Number::Text(t) => t.parse().unwrap_or(f64::NAN),
        }
    }

    /// The value as `i64` when it is a whole number in range.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Number::Int(i) => Some(*i),
            Number::Float(_) | Number::Text(_) => match Number::from_f64(self.as_f64())? {
                Number::Int(i) => Some(i),
                _ => None,
            },
        }
    }
}

impl From<i64> for Number {
    fn from(i: i64) -> Self {
        Number::Int(i)
    }
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
        write!(f, "[{}]", crate::write::quote(s))
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn number_literals() {
        let cases = [
            ("0", Some(Number::Int(0))),
            ("-42", Some(Number::Int(-42))),
            ("007", Some(Number::Int(7))),
            ("9223372036854775807", Some(Number::Int(i64::MAX))),
            (
                "9223372036854775808",
                Some(Number::Text("9223372036854775808".into())),
            ),
            ("1.5", Some(Number::Float(1.5))),
            ("-0.25", Some(Number::Float(-0.25))),
            ("1.50", Some(Number::Text("1.50".into()))),
            ("1.0", Some(Number::Text("1.0".into()))),
            ("1e10", Some(Number::Text("1e10".into()))),
            ("2.5E-3", Some(Number::Text("2.5E-3".into()))),
            ("1e+5", Some(Number::Text("1e+5".into()))),
            ("", None),
            ("-", None),
            (".5", None),
            ("1.", None),
            ("1e", None),
            ("+1", None),
            ("0x10", None),
            ("1 2", None),
            ("NaN", None),
        ];
        for (text, expected) in cases {
            assert_eq!(Number::from_literal(text), expected, "{text:?}");
        }
    }

    #[test]
    fn number_conversions() {
        assert_eq!(Number::from_f64(2.0), Some(Number::Int(2)));
        assert_eq!(Number::from_f64(-0.5), Some(Number::Float(-0.5)));
        assert_eq!(Number::from_f64(1e19), Some(Number::Float(1e19)));
        assert_eq!(Number::from_f64(f64::NAN), None);
        assert_eq!(Number::Text("1.50".into()).as_f64(), 1.5);
        assert_eq!(Number::Text("12".into()).as_i64(), Some(12));
        assert_eq!(Number::Float(3.0).as_i64(), Some(3));
        assert_eq!(Number::Float(3.5).as_i64(), None);
        assert_eq!(Number::Text("1e30".into()).as_i64(), None);
        assert_eq!(Number::from(7).to_string(), "7");
        assert_eq!(Number::Text("1.50".into()).to_string(), "1.50");
    }

    #[test]
    fn address_display_quotes_odd_labels() {
        let a = Address::new("table", vec!["app".into(), "odd.name".into()])
            .child("column", "2fa")
            .child("index", "x-y");
        assert_eq!(
            a.to_string(),
            r#"table.app["odd.name"].column["2fa"].index.x-y"#
        );
        let tricky = Address::new("role", vec!["a\"b ${c}".into()]);
        assert_eq!(tricky.to_string(), r#"role["a\"b $${c}"]"#);
        assert_eq!(a.leaf_kind(), "index");
        assert_eq!(a.leaf_label(), "x-y");
        assert_eq!(
            a.root(),
            Address::new("table", vec!["app".into(), "odd.name".into()])
        );
    }
}
