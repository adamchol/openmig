use crate::diag::{Diagnostic, Diagnostics};
use crate::doc::{Attr, Block, Document};
use crate::source::Span;
use crate::value::{Address, Number, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::BTreeSet;

/// Conversion from an evaluated [`Value`]. The error is a short description of what was expected,
/// e.g. `"a string"`.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

/// A closed set of bare identifiers, matched case-insensitively (`CASCADE`, `ROW`, `STABLE`).
/// Strings are accepted too.
pub trait Keyword: Sized {
    /// Human readable list of accepted values, e.g. `"ROW or STATEMENT"`.
    const EXPECTED: &'static str;
    fn parse(word: &str) -> Option<Self>;
}

/// Reads the attributes and nested blocks of one block, reporting problems with spans.
///
/// Every attribute that is never read is reported by [`BlockDecoder::finish`] as unknown, with a
/// did-you-mean suggestion drawn from the attribute names the decoder asked about.
pub struct BlockDecoder<'a> {
    block: &'a Block,
    doc: &'a Document,
    diags: &'a RefCell<Diagnostics>,
    asked: BTreeSet<String>,
    used: BTreeSet<String>,
    nested_used: BTreeSet<String>,
}

impl<'a> BlockDecoder<'a> {
    pub fn new(block: &'a Block, doc: &'a Document, diags: &'a RefCell<Diagnostics>) -> Self {
        Self {
            block,
            doc,
            diags,
            asked: BTreeSet::new(),
            used: BTreeSet::new(),
            nested_used: BTreeSet::new(),
        }
    }

    pub fn block(&self) -> &'a Block {
        self.block
    }

    pub fn doc(&self) -> &'a Document {
        self.doc
    }

    pub fn diagnostics(&self) -> &'a RefCell<Diagnostics> {
        self.diags
    }

    pub fn has(&self, name: &str) -> bool {
        self.block.attrs.contains_key(name)
    }

    /// The raw attribute, marked as used.
    pub fn attr(&mut self, name: &str) -> Option<&'a Attr> {
        self.asked.insert(name.to_owned());
        let attr = self.block.attrs.get(name)?;
        self.used.insert(name.to_owned());
        Some(attr)
    }

    /// The raw value, marked as used. `null` counts as absent.
    pub fn value(&mut self, name: &str) -> Option<&'a Value> {
        self.attr(name)
            .map(|a| &a.value)
            .filter(|v| !matches!(v, Value::Null))
    }

    /// A required attribute. Reports an error when it is missing or has the wrong type.
    pub fn req<T: FromValue>(&mut self, name: &str) -> Option<T> {
        if self.value(name).is_none() {
            self.error(
                self.block.header_span,
                format!("`{}` requires the attribute `{name}`", self.block.kind),
            );
            return None;
        }
        self.opt(name)
    }

    /// An optional attribute. Reports an error when it has the wrong type.
    pub fn opt<T: FromValue>(&mut self, name: &str) -> Option<T> {
        let attr = self.attr(name)?;
        if matches!(attr.value, Value::Null) {
            return None;
        }
        match T::from_value(&attr.value) {
            Ok(v) => Some(v),
            Err(expected) => {
                self.error(
                    attr.value_span,
                    format!(
                        "`{name}` must be {expected}, found {}",
                        attr.value.type_name()
                    ),
                );
                None
            }
        }
    }

    /// An optional keyword attribute (bare identifier or string, case-insensitive).
    pub fn keyword<K: Keyword>(&mut self, name: &str) -> Option<K> {
        let attr = self.attr(name)?;
        let word = match &attr.value {
            Value::Null => return None,
            Value::Ident(w) | Value::String(w) => w.as_str(),
            other => {
                self.error(
                    attr.value_span,
                    format!(
                        "`{name}` must be one of {}, found {}",
                        K::EXPECTED,
                        other.type_name()
                    ),
                );
                return None;
            }
        };
        match K::parse(word) {
            Some(k) => Some(k),
            None => {
                self.error(
                    attr.value_span,
                    format!("`{name}` must be one of {}, found `{word}`", K::EXPECTED),
                );
                None
            }
        }
    }

    /// Decoders for nested blocks of `kind`. Marks `kind` as understood.
    pub fn children(&mut self, kind: &str) -> Vec<BlockDecoder<'a>> {
        self.nested_used.insert(kind.to_owned());
        self.block
            .blocks
            .iter()
            .filter(|b| b.kind == kind)
            .map(|b| BlockDecoder::new(b, self.doc, self.diags))
            .collect()
    }

    pub fn error(&self, span: Span, message: impl Into<String>) {
        self.diags
            .borrow_mut()
            .push(Diagnostic::error(message).at(span));
    }

    pub fn warning(&self, span: Span, message: impl Into<String>) {
        self.diags
            .borrow_mut()
            .push(Diagnostic::warning(message).at(span));
    }

    pub fn push(&self, diagnostic: Diagnostic) {
        self.diags.borrow_mut().push(diagnostic);
    }

    /// Report attributes and nested blocks that were never read.
    pub fn finish(self) {
        for (name, attr) in &self.block.attrs {
            if self.used.contains(name) {
                continue;
            }
            let mut d = Diagnostic::error(format!(
                "unknown attribute `{name}` in `{}`",
                self.block.kind
            ))
            .at(attr.span);
            if let Some(s) = suggest(name, self.asked.iter().map(String::as_str)) {
                d = d.with_help(format!("did you mean `{s}`?"));
            }
            self.diags.borrow_mut().push(d);
        }
        for b in &self.block.blocks {
            if !self.nested_used.contains(&b.kind) {
                self.diags.borrow_mut().push(
                    Diagnostic::error(format!(
                        "unexpected block `{}` inside `{}`",
                        b.kind, self.block.kind
                    ))
                    .at(b.header_span),
                );
            }
        }
    }
}

/// Closest candidate by Jaro-Winkler similarity, if reasonably close.
pub fn suggest<'s>(word: &str, candidates: impl IntoIterator<Item = &'s str>) -> Option<&'s str> {
    candidates
        .into_iter()
        .map(|c| (strsim::jaro_winkler(word, c), c))
        .filter(|(score, _)| *score > 0.8)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, c)| c)
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(value.clone())
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            _ => Err("a string".into()),
        }
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(*b),
            _ => Err("a bool".into()),
        }
    }
}

impl FromValue for Number {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n.clone()),
            _ => Err("a number".into()),
        }
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(Number::Int(i)) => Ok(*i),
            _ => Err("an integer".into()),
        }
    }
}

impl FromValue for i32 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(Number::Int(i)) => {
                i32::try_from(*i).map_err(|_| "a 32-bit integer".into())
            }
            _ => Err("an integer".into()),
        }
    }
}

impl FromValue for u32 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(Number::Int(i)) => {
                u32::try_from(*i).map_err(|_| "a non-negative integer".into())
            }
            _ => Err("a non-negative integer".into()),
        }
    }
}

impl FromValue for Address {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Ref(a) => Ok(a.clone()),
            _ => Err("a reference".into()),
        }
    }
}

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(items) => items
                .iter()
                .map(T::from_value)
                .collect::<Result<_, _>>()
                .map_err(|e| format!("a list of {e}")),
            _ => match T::from_value(value) {
                Ok(_) => Err("a list (wrap the single value in [ ])".into()),
                Err(e) => Err(format!("a list of {e}")),
            },
        }
    }
}

impl<T: FromValue> FromValue for IndexMap<String, T> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| Ok((k.clone(), T::from_value(v)?)))
                .collect::<Result<_, String>>()
                .map_err(|e| format!("an object of {e}")),
            _ => Err("an object".into()),
        }
    }
}
