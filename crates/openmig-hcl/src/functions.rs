//! Built-in HCL functions available in schema files.
//!
//! Functions are pure, except `file()` which reads a file relative to the calling HCL file.
//! `try()` needs lazily evaluated arguments and is implemented by the evaluator. Calls of any
//! other name are kept as [`Value::Call`] for the engines.

use crate::diag::Diagnostic;
use crate::source::Span;
use crate::value::{Number, Value};
use indexmap::IndexMap;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Chomp,
    Coalesce,
    Concat,
    Contains,
    Distinct,
    File,
    Flatten,
    Format,
    Join,
    JsonDecode,
    JsonEncode,
    Keys,
    Length,
    Lower,
    Merge,
    Range,
    Replace,
    Split,
    Sql,
    ToBool,
    ToNumber,
    ToString,
    TrimPrefix,
    TrimSpace,
    TrimSuffix,
    Try,
    Upper,
    Values,
}

impl Builtin {
    const ALL: &[(&str, Builtin)] = &[
        ("chomp", Builtin::Chomp),
        ("coalesce", Builtin::Coalesce),
        ("concat", Builtin::Concat),
        ("contains", Builtin::Contains),
        ("distinct", Builtin::Distinct),
        ("file", Builtin::File),
        ("flatten", Builtin::Flatten),
        ("format", Builtin::Format),
        ("join", Builtin::Join),
        ("jsondecode", Builtin::JsonDecode),
        ("jsonencode", Builtin::JsonEncode),
        ("keys", Builtin::Keys),
        ("length", Builtin::Length),
        ("lower", Builtin::Lower),
        ("merge", Builtin::Merge),
        ("range", Builtin::Range),
        ("replace", Builtin::Replace),
        ("split", Builtin::Split),
        ("sql", Builtin::Sql),
        ("tobool", Builtin::ToBool),
        ("tonumber", Builtin::ToNumber),
        ("tostring", Builtin::ToString),
        ("trimprefix", Builtin::TrimPrefix),
        ("trimspace", Builtin::TrimSpace),
        ("trimsuffix", Builtin::TrimSuffix),
        ("try", Builtin::Try),
        ("upper", Builtin::Upper),
        ("values", Builtin::Values),
    ];

    pub(crate) fn lookup(name: &str) -> Option<Builtin> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
    }

    pub(crate) fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, b)| *b == self)
            .map_or("", |(n, _)| n)
    }
}

/// An evaluated argument and the span of its expression.
pub(crate) struct Arg {
    pub value: Value,
    pub span: Span,
}

pub(crate) struct CallSite<'p> {
    pub span: Span,
    /// Directory of the calling file, for `file()`.
    pub base_dir: Option<&'p Path>,
}

type Res<T = Value> = Result<T, Diagnostic>;

pub(crate) fn call(builtin: Builtin, args: Vec<Arg>, site: &CallSite<'_>) -> Res {
    let c = Call {
        name: builtin.name(),
        args,
        site,
    };
    match builtin {
        Builtin::Chomp => {
            c.arity(1, 1)?;
            Ok(Value::String(
                c.string(0)?.trim_end_matches(['\n', '\r']).to_owned(),
            ))
        }
        Builtin::Coalesce => {
            c.arity(1, usize::MAX)?;
            c.args
                .iter()
                .map(|a| &a.value)
                .find(|v| !matches!(v, Value::Null) && v.as_str() != Some(""))
                .cloned()
                .ok_or_else(|| c.error("all arguments are null or empty strings"))
        }
        Builtin::Concat => {
            let mut out = Vec::new();
            for i in 0..c.args.len() {
                out.extend_from_slice(c.list(i)?);
            }
            Ok(Value::List(out))
        }
        Builtin::Contains => {
            c.arity(2, 2)?;
            let needle = &c.args[1].value;
            Ok(Value::Bool(c.list(0)?.contains(needle)))
        }
        Builtin::Distinct => {
            c.arity(1, 1)?;
            let mut out: Vec<Value> = Vec::new();
            for v in c.list(0)? {
                if !out.contains(v) {
                    out.push(v.clone());
                }
            }
            Ok(Value::List(out))
        }
        Builtin::File => {
            c.arity(1, 1)?;
            let rel = c.string(0)?;
            let path = match site.base_dir {
                Some(dir) => dir.join(&rel),
                None => Path::new(&rel).to_path_buf(),
            };
            std::fs::read_to_string(&path)
                .map(Value::String)
                .map_err(|e| {
                    Diagnostic::error(format!("cannot read `{}`: {e}", path.display()))
                        .at(c.args[0].span)
                })
        }
        Builtin::Flatten => {
            c.arity(1, 1)?;
            let mut out = Vec::new();
            flatten_into(c.list(0)?, &mut out);
            Ok(Value::List(out))
        }
        Builtin::Format => {
            c.arity(1, usize::MAX)?;
            format(&c).map(Value::String)
        }
        Builtin::Join => {
            c.arity(2, 2)?;
            let sep = c.string(0)?;
            let parts = c
                .list(1)?
                .iter()
                .map(|v| primitive_text(v).ok_or_else(|| c.arg_error(1, "a list of strings", v)))
                .collect::<Res<Vec<_>>>()?;
            Ok(Value::String(parts.join(&sep)))
        }
        Builtin::JsonDecode => {
            c.arity(1, 1)?;
            let json: serde_json::Value = serde_json::from_str(&c.string(0)?).map_err(|e| {
                Diagnostic::error(format!("`jsondecode`: invalid JSON: {e}")).at(c.args[0].span)
            })?;
            Ok(json_to_value(json))
        }
        Builtin::JsonEncode => {
            c.arity(1, 1)?;
            let mut out = String::new();
            write_json(&mut out, &c.args[0].value)
                .map_err(|e| Diagnostic::error(format!("`jsonencode`: {e}")).at(c.args[0].span))?;
            Ok(Value::String(out))
        }
        Builtin::Keys => {
            c.arity(1, 1)?;
            Ok(Value::List(
                c.object(0)?.keys().cloned().map(Value::String).collect(),
            ))
        }
        Builtin::Length => {
            c.arity(1, 1)?;
            let len = match &c.args[0].value {
                Value::String(s) => s.chars().count(),
                Value::List(items) => items.len(),
                Value::Object(map) => map.len(),
                other => return Err(c.arg_error(0, "a string, list or object", other)),
            };
            Ok(Value::Number(Number::Int(len as i64)))
        }
        Builtin::Lower => {
            c.arity(1, 1)?;
            Ok(Value::String(c.string(0)?.to_lowercase()))
        }
        Builtin::Merge => {
            let mut out = IndexMap::new();
            for (i, arg) in c.args.iter().enumerate() {
                match &arg.value {
                    Value::Null => {}
                    Value::Object(map) => {
                        out.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                    other => return Err(c.arg_error(i, "an object", other)),
                }
            }
            Ok(Value::Object(out))
        }
        Builtin::Range => range(&c),
        Builtin::Replace => {
            c.arity(3, 3)?;
            let (s, from, to) = (c.string(0)?, c.string(1)?, c.string(2)?);
            if from.len() > 1 && from.starts_with('/') && from.ends_with('/') {
                return Err(Diagnostic::error(
                    "`replace`: regular expressions (`/.../`) are not supported",
                )
                .at(c.args[1].span));
            }
            if from.is_empty() {
                return Err(c.error("the substring to replace cannot be empty"));
            }
            Ok(Value::String(s.replace(&from, &to)))
        }
        Builtin::Split => {
            c.arity(2, 2)?;
            let (sep, s) = (c.string(0)?, c.string(1)?);
            if sep.is_empty() {
                return Err(c.error("the separator cannot be empty"));
            }
            Ok(Value::List(
                s.split(&sep).map(|p| Value::String(p.to_owned())).collect(),
            ))
        }
        Builtin::Sql => {
            c.arity(1, 1)?;
            match &c.args[0].value {
                Value::String(s) => Ok(Value::Sql(s.clone())),
                other => Err(c.arg_error(0, "a string", other)),
            }
        }
        Builtin::ToBool => {
            c.arity(1, 1)?;
            match &c.args[0].value {
                v @ (Value::Null | Value::Bool(_)) => Ok(v.clone()),
                Value::String(s) if s == "true" => Ok(Value::Bool(true)),
                Value::String(s) if s == "false" => Ok(Value::Bool(false)),
                other => Err(c.arg_error(0, "a bool or \"true\"/\"false\"", other)),
            }
        }
        Builtin::ToNumber => {
            c.arity(1, 1)?;
            match &c.args[0].value {
                v @ (Value::Null | Value::Number(_)) => Ok(v.clone()),
                Value::String(s) => Number::from_literal(s.trim())
                    .map(Value::Number)
                    .ok_or_else(|| {
                        Diagnostic::error(format!("`tonumber`: `{s}` is not a number"))
                            .at(c.args[0].span)
                    }),
                other => Err(c.arg_error(0, "a number or a numeric string", other)),
            }
        }
        Builtin::ToString => {
            c.arity(1, 1)?;
            match &c.args[0].value {
                Value::Null => Ok(Value::Null),
                v => primitive_text(v)
                    .map(Value::String)
                    .ok_or_else(|| c.arg_error(0, "a string, number or bool", v)),
            }
        }
        Builtin::TrimPrefix => {
            c.arity(2, 2)?;
            let (s, prefix) = (c.string(0)?, c.string(1)?);
            Ok(Value::String(
                s.strip_prefix(prefix.as_str()).unwrap_or(&s).to_owned(),
            ))
        }
        Builtin::TrimSpace => {
            c.arity(1, 1)?;
            Ok(Value::String(c.string(0)?.trim().to_owned()))
        }
        Builtin::TrimSuffix => {
            c.arity(2, 2)?;
            let (s, suffix) = (c.string(0)?, c.string(1)?);
            Ok(Value::String(
                s.strip_suffix(suffix.as_str()).unwrap_or(&s).to_owned(),
            ))
        }
        Builtin::Try => Err(c.error("`try` is evaluated lazily by the evaluator")),
        Builtin::Upper => {
            c.arity(1, 1)?;
            Ok(Value::String(c.string(0)?.to_uppercase()))
        }
        Builtin::Values => {
            c.arity(1, 1)?;
            Ok(Value::List(c.object(0)?.values().cloned().collect()))
        }
    }
}

struct Call<'c> {
    name: &'static str,
    args: Vec<Arg>,
    site: &'c CallSite<'c>,
}

impl Call<'_> {
    fn error(&self, message: impl std::fmt::Display) -> Diagnostic {
        Diagnostic::error(format!("`{}`: {message}", self.name)).at(self.site.span)
    }

    fn arg_error(&self, i: usize, expected: &str, found: &Value) -> Diagnostic {
        Diagnostic::error(format!(
            "`{}`: argument {} must be {expected}, found {}",
            self.name,
            i + 1,
            found.type_name()
        ))
        .at(self.args[i].span)
    }

    fn arity(&self, min: usize, max: usize) -> Res<()> {
        let n = self.args.len();
        if (min..=max).contains(&n) {
            return Ok(());
        }
        let expected = match (min, max) {
            (min, max) if min == max => format!("{min} argument{}", plural(min)),
            (min, usize::MAX) => format!("at least {min} argument{}", plural(min)),
            (min, max) => format!("{min} to {max} arguments"),
        };
        Err(
            Diagnostic::error(format!("`{}` takes {expected}, got {n}", self.name))
                .at(self.site.span),
        )
    }

    /// A string argument; numbers and bools are converted.
    fn string(&self, i: usize) -> Res<String> {
        let v = &self.args[i].value;
        primitive_text(v).ok_or_else(|| self.arg_error(i, "a string", v))
    }

    fn list(&self, i: usize) -> Res<&[Value]> {
        match &self.args[i].value {
            Value::List(items) => Ok(items),
            other => Err(self.arg_error(i, "a list", other)),
        }
    }

    fn object(&self, i: usize) -> Res<&IndexMap<String, Value>> {
        match &self.args[i].value {
            Value::Object(map) => Ok(map),
            other => Err(self.arg_error(i, "an object", other)),
        }
    }

    fn number(&self, i: usize) -> Res<&Number> {
        match &self.args[i].value {
            Value::Number(n) => Ok(n),
            other => Err(self.arg_error(i, "a number", other)),
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Strings as is; numbers and bools converted to text.
fn primitive_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn flatten_into(items: &[Value], out: &mut Vec<Value>) {
    for item in items {
        match item {
            Value::List(inner) => flatten_into(inner, out),
            other => out.push(other.clone()),
        }
    }
}

const MAX_RANGE: usize = 1 << 20;

fn range(c: &Call<'_>) -> Res {
    c.arity(1, 3)?;
    let numbers = (0..c.args.len())
        .map(|i| c.number(i).cloned())
        .collect::<Res<Vec<_>>>()?;
    let (start, limit, step) = match numbers.as_slice() {
        [limit] => (Number::Int(0), limit.clone(), None),
        [start, limit] => (start.clone(), limit.clone(), None),
        [start, limit, step] => (start.clone(), limit.clone(), Some(step.clone())),
        _ => unreachable!("arity is checked"),
    };
    let (start, limit) = (start.as_f64(), limit.as_f64());
    let step = match step {
        Some(step) => step.as_f64(),
        None if start <= limit => 1.0,
        None => -1.0,
    };
    if step == 0.0 || !step.is_finite() {
        return Err(c.error("the step cannot be zero"));
    }
    if (step > 0.0 && start > limit) || (step < 0.0 && start < limit) {
        return Err(c.error("the step must move from the start towards the limit"));
    }
    let count = ((limit - start) / step).ceil();
    if !count.is_finite() || count > MAX_RANGE as f64 {
        return Err(c.error(format!("the range has more than {MAX_RANGE} elements")));
    }
    let values = (0..count as usize)
        .filter_map(|i| Number::from_f64(start + step * i as f64))
        .map(Value::Number)
        .collect();
    Ok(Value::List(values))
}

/// Go-style formatting: `%s %d %f %t %v %q %%`, with `-`, `0` and `+` flags, width and precision.
fn format(c: &Call<'_>) -> Res<String> {
    let template = c.string(0)?;
    let mut out = String::new();
    let mut next = 1;
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let (mut left, mut zero, mut plus) = (false, false, false);
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                _ => break,
            }
            chars.next();
        }
        let width = digits(&mut chars);
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            Some(digits(&mut chars).unwrap_or(0))
        } else {
            None
        };
        let Some(verb) = chars.next() else {
            return Err(c.error("the format string ends with an incomplete verb"));
        };
        if verb == '%' {
            out.push('%');
            continue;
        }
        let Some(arg) = c.args.get(next) else {
            return Err(c.error(format!(
                "not enough arguments for the format string ({} given)",
                c.args.len() - 1
            )));
        };
        let index = next;
        next += 1;
        let value = &arg.value;
        let numeric = matches!(verb, 'd' | 'f');
        let mut text = match verb {
            's' => primitive_text(value).ok_or_else(|| c.arg_error(index, "a string", value))?,
            'd' => match value {
                Value::Number(n) => match n {
                    Number::Int(i) => i.to_string(),
                    Number::Text(t) if !t.contains(['.', 'e', 'E']) => t.clone(),
                    _ => n
                        .as_i64()
                        .map(|i| i.to_string())
                        .ok_or_else(|| c.arg_error(index, "a whole number", value))?,
                },
                other => return Err(c.arg_error(index, "a number", other)),
            },
            'f' => match value {
                Value::Number(n) => format!("{:.*}", precision.unwrap_or(6), n.as_f64()),
                other => return Err(c.arg_error(index, "a number", other)),
            },
            't' => match value {
                Value::Bool(b) => b.to_string(),
                other => return Err(c.arg_error(index, "a bool", other)),
            },
            'v' => match value {
                Value::Null => "null".to_owned(),
                Value::Ident(word) => word.clone(),
                Value::List(_) | Value::Object(_) => {
                    let mut json = String::new();
                    write_json(&mut json, value).map_err(|e| c.error(e))?;
                    json
                }
                v => primitive_text(v).ok_or_else(|| c.arg_error(index, "a printable value", v))?,
            },
            'q' => {
                let s =
                    primitive_text(value).ok_or_else(|| c.arg_error(index, "a string", value))?;
                serde_json::Value::String(s).to_string()
            }
            other => return Err(c.error(format!("unsupported verb `%{other}`"))),
        };
        if numeric && plus && !text.starts_with('-') {
            text.insert(0, '+');
        }
        if verb != 'f'
            && let Some(p) = precision
        {
            text = text.chars().take(p).collect();
        }
        if let Some(width) = width {
            let len = text.chars().count();
            if len < width {
                let pad = width - len;
                if left {
                    text.extend(std::iter::repeat_n(' ', pad));
                } else if zero && numeric {
                    let sign = usize::from(text.starts_with(['-', '+']));
                    text.insert_str(sign, &"0".repeat(pad));
                } else {
                    text.insert_str(0, &" ".repeat(pad));
                }
            }
        }
        out.push_str(&text);
    }
    if next < c.args.len() {
        return Err(c.error(format!(
            "too many arguments for the format string ({} used, {} given)",
            next - 1,
            c.args.len() - 1
        )));
    }
    Ok(out)
}

fn digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<usize> {
    let mut n: Option<usize> = None;
    while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
        n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d as usize));
        chars.next();
    }
    n
}

/// Converts decoded JSON into a [`Value`]. Numbers keep their JSON text when it is not an exact
/// `i64`/`f64`.
pub(crate) fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            let text = n.to_string();
            Value::Number(Number::from_literal(&text).unwrap_or(Number::Text(text)))
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(items) => {
            Value::List(items.into_iter().map(json_to_value).collect())
        }
        serde_json::Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

/// Compact JSON. References, identifiers, calls and `sql()` values have no JSON form.
fn write_json(out: &mut String, value: &Value) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let _ = write!(out, "{n}");
        }
        Value::String(s) => out.push_str(&serde_json::Value::String(s.clone()).to_string()),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(k.clone()).to_string());
                out.push(':');
                write_json(out, v)?;
            }
            out.push('}');
        }
        Value::Ref(_) | Value::Ident(_) | Value::Call { .. } | Value::Sql(_) => {
            return Err(format!("{} values have no JSON form", value.type_name()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FileId;
    use pretty_assertions::assert_eq;

    fn site() -> CallSite<'static> {
        CallSite {
            span: Span::new(FileId(0), 0, 1),
            base_dir: None,
        }
    }

    fn run(name: &str, args: Vec<Value>) -> Res {
        let builtin = Builtin::lookup(name).expect("known function");
        let args = args
            .into_iter()
            .enumerate()
            .map(|(i, value)| Arg {
                value,
                span: Span::new(FileId(0), i + 10, i + 11),
            })
            .collect();
        call(builtin, args, &site())
    }

    fn ok(name: &str, args: Vec<Value>) -> Value {
        run(name, args).unwrap_or_else(|d| panic!("{name} failed: {}", d.message))
    }

    fn err(name: &str, args: Vec<Value>) -> String {
        run(name, args).expect_err("an error").message
    }

    fn s(v: &str) -> Value {
        Value::String(v.to_owned())
    }

    fn n(i: i64) -> Value {
        Value::Number(Number::Int(i))
    }

    fn list(items: Vec<Value>) -> Value {
        Value::List(items)
    }

    fn obj(entries: &[(&str, Value)]) -> Value {
        Value::Object(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn names_round_trip() {
        for (name, builtin) in Builtin::ALL {
            assert_eq!(Builtin::lookup(name), Some(*builtin));
            assert_eq!(builtin.name(), *name);
        }
        assert_eq!(Builtin::lookup("varchar"), None);
    }

    #[test]
    fn format_verbs() {
        assert_eq!(
            ok(
                "format",
                vec![
                    s("%s-%d-%v-%q-%%"),
                    s("a"),
                    n(42),
                    Value::Bool(true),
                    s("x\"y")
                ]
            ),
            s(r#"a-42-true-"x\"y"-%"#)
        );
        assert_eq!(
            ok("format", vec![s("%05d|%-4s|%4s"), n(-42), s("ab"), s("cd")]),
            s("-0042|ab  |  cd")
        );
        assert_eq!(
            ok(
                "format",
                vec![s("%.2f %+d"), Value::Number(Number::Float(1.23456)), n(5)]
            ),
            s("1.23 +5")
        );
        assert_eq!(
            ok("format", vec![s("%v"), list(vec![n(1), s("a")])]),
            s(r#"[1,"a"]"#)
        );
        assert_eq!(
            ok(
                "format",
                vec![s("%v %v"), Value::Null, Value::Ident("ROW".into())]
            ),
            s("null ROW")
        );
        assert_eq!(ok("format", vec![s("%s"), n(7)]), s("7"));
        assert_eq!(ok("format", vec![s("%t"), Value::Bool(false)]), s("false"));
        assert_eq!(ok("format", vec![s("%.3s"), s("abcdef")]), s("abc"));
        assert!(err("format", vec![s("%s %s"), s("a")]).contains("not enough arguments"));
        assert!(err("format", vec![s("%s"), s("a"), s("b")]).contains("too many arguments"));
        assert!(err("format", vec![s("%d"), s("a")]).contains("argument 2 must be a number"));
        assert!(
            err("format", vec![s("%d"), Value::Number(Number::Float(1.5))])
                .contains("whole number")
        );
        assert!(err("format", vec![s("%x"), n(1)]).contains("unsupported verb `%x`"));
        assert!(err("format", vec![s("abc %")]).contains("incomplete verb"));
        assert!(err("format", vec![]).contains("takes at least 1 argument, got 0"));
    }

    #[test]
    fn string_functions() {
        assert_eq!(
            ok(
                "join",
                vec![s(", "), list(vec![s("a"), n(1), Value::Bool(true)])]
            ),
            s("a, 1, true")
        );
        assert!(err("join", vec![s(","), list(vec![Value::Null])]).contains("a list of strings"));
        assert_eq!(
            ok("split", vec![s(","), s("a,b,,c")]),
            list(vec![s("a"), s("b"), s(""), s("c")])
        );
        assert!(err("split", vec![s(""), s("a")]).contains("separator cannot be empty"));
        assert_eq!(ok("lower", vec![s("ÀBC")]), s("àbc"));
        assert_eq!(ok("upper", vec![s("àbc")]), s("ÀBC"));
        assert_eq!(ok("replace", vec![s("a.b.c"), s("."), s("_")]), s("a_b_c"));
        assert!(err("replace", vec![s("abc"), s("/b/"), s("x")]).contains("regular expressions"));
        assert_eq!(ok("trimspace", vec![s("  a b \n")]), s("a b"));
        assert_eq!(
            ok("trimprefix", vec![s("app_users"), s("app_")]),
            s("users")
        );
        assert_eq!(ok("trimprefix", vec![s("users"), s("app_")]), s("users"));
        assert_eq!(ok("trimsuffix", vec![s("users_v2"), s("_v2")]), s("users"));
        assert_eq!(ok("chomp", vec![s("a\nb\n\r\n")]), s("a\nb"));
        assert!(err("lower", vec![s("a"), s("b")]).contains("takes 1 argument, got 2"));
        assert!(
            err("upper", vec![list(vec![])]).contains("argument 1 must be a string, found list")
        );
    }

    #[test]
    fn collection_functions() {
        assert_eq!(
            ok(
                "concat",
                vec![list(vec![n(1)]), list(vec![]), list(vec![n(2), n(3)])]
            ),
            list(vec![n(1), n(2), n(3)])
        );
        assert!(err("concat", vec![list(vec![]), s("x")]).contains("argument 2 must be a list"));
        assert_eq!(
            ok(
                "flatten",
                vec![list(vec![
                    n(1),
                    list(vec![n(2), list(vec![n(3)])]),
                    list(vec![])
                ])]
            ),
            list(vec![n(1), n(2), n(3)])
        );
        assert_eq!(
            ok("distinct", vec![list(vec![s("a"), s("b"), s("a")])]),
            list(vec![s("a"), s("b")])
        );
        assert_eq!(
            ok("contains", vec![list(vec![s("a")]), s("a")]),
            Value::Bool(true)
        );
        assert_eq!(
            ok("contains", vec![list(vec![s("a")]), s("b")]),
            Value::Bool(false)
        );
        let o = obj(&[("b", n(1)), ("a", n(2))]);
        assert_eq!(ok("keys", vec![o.clone()]), list(vec![s("b"), s("a")]));
        assert_eq!(ok("values", vec![o.clone()]), list(vec![n(1), n(2)]));
        assert!(err("keys", vec![list(vec![])]).contains("must be an object"));
        assert_eq!(
            ok(
                "merge",
                vec![o, Value::Null, obj(&[("a", n(3)), ("c", n(4))])]
            ),
            obj(&[("b", n(1)), ("a", n(3)), ("c", n(4))])
        );
        assert!(err("merge", vec![n(1)]).contains("must be an object"));
        assert_eq!(ok("length", vec![s("héllo")]), n(5));
        assert_eq!(ok("length", vec![list(vec![n(1), n(2)])]), n(2));
        assert_eq!(ok("length", vec![obj(&[("a", n(1))])]), n(1));
        assert!(err("length", vec![n(1)]).contains("a string, list or object"));
        assert_eq!(
            ok("coalesce", vec![Value::Null, s(""), s("x"), s("y")]),
            s("x")
        );
        assert!(err("coalesce", vec![Value::Null]).contains("all arguments are null"));
    }

    #[test]
    fn ranges() {
        assert_eq!(ok("range", vec![n(3)]), list(vec![n(0), n(1), n(2)]));
        assert_eq!(ok("range", vec![n(1), n(4)]), list(vec![n(1), n(2), n(3)]));
        assert_eq!(ok("range", vec![n(4), n(1)]), list(vec![n(4), n(3), n(2)]));
        assert_eq!(
            ok("range", vec![n(0), n(10), n(4)]),
            list(vec![n(0), n(4), n(8)])
        );
        assert_eq!(
            ok(
                "range",
                vec![n(0), n(1), Value::Number(Number::Float(0.25))]
            ),
            list(vec![
                n(0),
                Value::Number(Number::Float(0.25)),
                Value::Number(Number::Float(0.5)),
                Value::Number(Number::Float(0.75)),
            ])
        );
        assert_eq!(ok("range", vec![n(0)]), list(vec![]));
        assert!(err("range", vec![n(0), n(5), n(0)]).contains("step cannot be zero"));
        assert!(err("range", vec![n(0), n(5), n(-1)]).contains("towards the limit"));
        assert!(err("range", vec![n(10_000_000)]).contains("more than"));
        assert!(err("range", vec![s("a")]).contains("must be a number"));
    }

    #[test]
    fn conversions() {
        assert_eq!(ok("tostring", vec![n(5)]), s("5"));
        assert_eq!(ok("tostring", vec![Value::Bool(true)]), s("true"));
        assert_eq!(ok("tostring", vec![Value::Null]), Value::Null);
        assert!(err("tostring", vec![list(vec![])]).contains("a string, number or bool"));
        assert_eq!(
            ok("tonumber", vec![s("1.50")]),
            Value::Number(Number::Text("1.50".into()))
        );
        assert_eq!(ok("tonumber", vec![s(" 12 ")]), n(12));
        assert!(err("tonumber", vec![s("x")]).contains("`x` is not a number"));
        assert_eq!(ok("tobool", vec![s("false")]), Value::Bool(false));
        assert_eq!(ok("tobool", vec![Value::Bool(true)]), Value::Bool(true));
        assert!(err("tobool", vec![s("yes")]).contains("must be a bool"));
        assert_eq!(ok("sql", vec![s("now()")]), Value::Sql("now()".into()));
        assert!(err("sql", vec![n(1)]).contains("must be a string"));
    }

    #[test]
    fn json() {
        let v = obj(&[
            (
                "a",
                list(vec![
                    n(1),
                    Value::Number(Number::Text("1.50".into())),
                    Value::Null,
                ]),
            ),
            ("b", s("x\"\n")),
            ("c", Value::Bool(false)),
        ]);
        let text = ok("jsonencode", vec![v]);
        assert_eq!(text, s(r#"{"a":[1,1.50,null],"b":"x\"\n","c":false}"#));
        assert_eq!(
            ok("jsondecode", vec![text]),
            obj(&[
                (
                    "a",
                    list(vec![n(1), Value::Number(Number::Float(1.5)), Value::Null])
                ),
                ("b", s("x\"\n")),
                ("c", Value::Bool(false)),
            ])
        );
        assert_eq!(
            ok("jsondecode", vec![s("123456789012345678901234567890")]),
            Value::Number(Number::Text("1.2345678901234568e+29".into()))
        );
        assert!(err("jsondecode", vec![s("{")]).contains("invalid JSON"));
        assert!(
            err("jsonencode", vec![Value::Ident("x".into())])
                .contains("identifier values have no JSON form")
        );
    }

    #[test]
    fn file_reads_relative_to_the_calling_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("body.sql"), "SELECT 1;\n").unwrap();
        let site = CallSite {
            span: Span::default(),
            base_dir: Some(dir.path()),
        };
        let arg = |p: &str| Arg {
            value: s(p),
            span: Span::default(),
        };
        assert_eq!(
            call(Builtin::File, vec![arg("body.sql")], &site).unwrap(),
            s("SELECT 1;\n")
        );
        let e = call(Builtin::File, vec![arg("missing.sql")], &site).unwrap_err();
        assert!(e.message.starts_with("cannot read"), "{}", e.message);
    }
}
