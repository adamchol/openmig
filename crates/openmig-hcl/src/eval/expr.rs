//! Expression evaluation: literals, collections, operators, conditionals, `for` expressions and
//! function calls. Traversals are resolved in [`super::refs`], templates in [`super::template`].

use super::collect::NodeId;
use super::{Loader, span};
use crate::diag::Diagnostic;
use crate::functions::{self, Arg, Builtin, CallSite};
use crate::source::{FileId, Span};
use crate::value::{Number, Value};
use hcl_edit::Span as _;
use hcl_edit::expr::{
    BinaryOp, BinaryOperator, Expression, ForExpr, FuncCall, ObjectKey, Traversal,
    TraversalOperator, UnaryOp, UnaryOperator,
};
use indexmap::IndexMap;
use indexmap::map::Entry;

/// Marks a failed evaluation. The reason has already been reported as a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fail;

pub(super) type Eval<T = Value> = Result<T, Fail>;

/// The current `for_each` instance.
pub(super) struct Each {
    pub key: Value,
    pub value: Value,
}

/// A variable bound by a `for` expression or template directive.
pub(super) struct Binding<'s> {
    name: &'s str,
    value: Value,
    next: Option<&'s Binding<'s>>,
}

/// Where an expression is evaluated.
#[derive(Clone, Copy)]
pub(super) struct Scope<'s> {
    /// File of the expression: spans and `file()` paths are relative to it.
    pub file: FileId,
    /// Innermost enclosing block, for relative references like `column.id`.
    pub block: Option<NodeId>,
    pub each: Option<&'s Each>,
    pub bindings: Option<&'s Binding<'s>>,
    /// Evaluating a variable default: `var` and `local` are not available.
    pub var_default: bool,
}

impl<'s> Scope<'s> {
    pub fn new(file: FileId) -> Self {
        Self {
            file,
            block: None,
            each: None,
            bindings: None,
            var_default: false,
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&'s Value> {
        let mut binding = self.bindings;
        while let Some(b) = binding {
            if b.name == name {
                return Some(&b.value);
            }
            binding = b.next;
        }
        None
    }

    pub fn span(&self, node: &impl hcl_edit::Span) -> Span {
        span(self.file, node.span())
    }
}

/// Binds `key_var` (optional) and `value_var` for one iteration and runs `f` in the new scope.
pub(super) fn with_bindings<T>(
    scope: &Scope<'_>,
    key_var: Option<&str>,
    key: Value,
    value_var: &str,
    value: Value,
    f: impl FnOnce(&Scope<'_>) -> T,
) -> T {
    let key_binding;
    let mut inner = *scope;
    if let Some(name) = key_var {
        key_binding = Binding {
            name,
            value: key,
            next: inner.bindings,
        };
        inner.bindings = Some(&key_binding);
    }
    let value_binding = Binding {
        name: value_var,
        value,
        next: inner.bindings,
    };
    inner.bindings = Some(&value_binding);
    f(&inner)
}

/// The `(key, value)` pairs a `for` expression or `for` directive iterates over.
pub(super) fn iteration_items(collection: Value) -> Result<Vec<(Value, Value)>, Value> {
    match collection {
        Value::List(items) => Ok(items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Value::Number((i as i64).into()), v))
            .collect()),
        Value::Object(map) => Ok(map
            .into_iter()
            .map(|(k, v)| (Value::String(k), v))
            .collect()),
        other => Err(other),
    }
}

impl Loader<'_> {
    pub(super) fn eval(&self, expr: &Expression, scope: &Scope<'_>) -> Eval {
        match expr {
            Expression::Null(_) => Ok(Value::Null),
            Expression::Bool(b) => Ok(Value::Bool(*b.value())),
            Expression::Number(n) => Ok(Value::Number(self.number_literal(n, scope))),
            Expression::String(s) => Ok(Value::String(s.value().clone())),
            Expression::Array(array) => self.eval_all(array.iter(), scope).map(Value::List),
            Expression::Object(object) => {
                let mut map = IndexMap::new();
                let mut failed = false;
                for (key, value) in object.iter() {
                    let key = match key {
                        ObjectKey::Ident(ident) => Ok(ident.as_str().to_owned()),
                        ObjectKey::Expression(expr) => self.object_key(expr, scope),
                    };
                    match (key, self.eval(value.expr(), scope)) {
                        (Ok(k), Ok(v)) => match map.entry(k) {
                            Entry::Vacant(slot) => {
                                slot.insert(v);
                            }
                            Entry::Occupied(slot) => {
                                failed = true;
                                self.error(
                                    Diagnostic::error(format!(
                                        "duplicate object key `{}`",
                                        slot.key()
                                    ))
                                    .at(scope.span(value.expr())),
                                );
                            }
                        },
                        _ => failed = true,
                    }
                }
                if failed {
                    Err(Fail)
                } else {
                    Ok(Value::Object(map))
                }
            }
            Expression::StringTemplate(t) => self.eval_template(t, scope).map(Value::String),
            Expression::HeredocTemplate(h) => {
                self.eval_template(&h.template, scope).map(Value::String)
            }
            Expression::Parenthesis(p) => self.eval(p.inner(), scope),
            Expression::Variable(name) => {
                self.resolve_path(name.as_str(), scope.span(name), &[], scope)
            }
            Expression::Conditional(c) => match self.eval(&c.cond_expr, scope)? {
                Value::Bool(true) => self.eval(&c.true_expr, scope),
                Value::Bool(false) => self.eval(&c.false_expr, scope),
                other => Err(self.error(
                    Diagnostic::error(format!(
                        "the condition must be a bool, found {}",
                        other.type_name()
                    ))
                    .at(scope.span(&c.cond_expr)),
                )),
            },
            Expression::FuncCall(call) => self.call(call, scope),
            Expression::Traversal(t) => self.traversal(t, scope),
            Expression::UnaryOp(op) => self.unary(op, scope),
            Expression::BinaryOp(op) => self.binary(op, scope),
            Expression::ForExpr(f) => self.for_expr(f, scope),
        }
    }

    fn eval_all<'e>(
        &self,
        exprs: impl Iterator<Item = &'e Expression>,
        scope: &Scope<'_>,
    ) -> Eval<Vec<Value>> {
        let mut values = Vec::new();
        let mut failed = false;
        for expr in exprs {
            match self.eval(expr, scope) {
                Ok(v) => values.push(v),
                Err(_) => failed = true,
            }
        }
        if failed { Err(Fail) } else { Ok(values) }
    }

    fn number_literal(
        &self,
        number: &hcl_edit::Formatted<hcl_edit::Number>,
        scope: &Scope<'_>,
    ) -> Number {
        let text: String = number
            .span()
            .and_then(|r| self.sources.get(scope.file).content.get(r))
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        Number::from_literal(&text).unwrap_or_else(|| {
            let n = number.value();
            match n.as_i64() {
                Some(i) => Number::Int(i),
                None => Number::from_f64(n.as_f64().unwrap_or_default())
                    .unwrap_or(Number::Text(n.to_string())),
            }
        })
    }

    fn object_key(&self, expr: &Expression, scope: &Scope<'_>) -> Eval<String> {
        match self.eval(expr, scope)? {
            Value::String(s) => Ok(s),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            other => Err(self.error(
                Diagnostic::error(format!(
                    "object keys must be strings, found {}",
                    other.type_name()
                ))
                .at(scope.span(expr)),
            )),
        }
    }

    fn traversal(&self, t: &Traversal, scope: &Scope<'_>) -> Eval {
        let mut segments = Vec::new();
        let mut failed = false;
        for op in &t.operators {
            let at = scope.span(op);
            let key = match op.value() {
                TraversalOperator::GetAttr(name) => Key::Name(name.as_str().to_owned()),
                TraversalOperator::LegacyIndex(i) => Key::Value(Value::Number(
                    i64::try_from(*i.value()).map_or(Number::Text(i.to_string()), Number::Int),
                )),
                TraversalOperator::Index(expr) => match self.eval(expr, scope) {
                    Ok(v) => Key::Value(v),
                    Err(_) => {
                        failed = true;
                        continue;
                    }
                },
                TraversalOperator::AttrSplat(_) | TraversalOperator::FullSplat(_) => {
                    return Err(self.error(
                        Diagnostic::error("splat expressions are not supported")
                            .at(at)
                            .with_help("use a `for` expression: `[for x in list : x.attr]`"),
                    ));
                }
            };
            segments.push(Segment { key, span: at });
        }
        if failed {
            return Err(Fail);
        }
        match &t.expr {
            Expression::Variable(root) => {
                self.resolve_path(root.as_str(), scope.span(root), &segments, scope)
            }
            other => {
                let base = self.eval(other, scope)?;
                self.index_value(base, &segments)
            }
        }
    }

    fn unary(&self, op: &UnaryOp, scope: &Scope<'_>) -> Eval {
        let value = self.eval(&op.expr, scope)?;
        let at = scope.span(op);
        match (op.operator.value(), value) {
            (UnaryOperator::Neg, Value::Number(n)) => Ok(Value::Number(negate(n))),
            (UnaryOperator::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnaryOperator::Neg, other) => Err(self.error(
                Diagnostic::error(format!("`-` needs a number, found {}", other.type_name()))
                    .at(at),
            )),
            (UnaryOperator::Not, other) => Err(self.error(
                Diagnostic::error(format!("`!` needs a bool, found {}", other.type_name())).at(at),
            )),
        }
    }

    fn binary(&self, op: &BinaryOp, scope: &Scope<'_>) -> Eval {
        let operator = *op.operator.value();
        let at = scope.span(op);
        if matches!(operator, BinaryOperator::And | BinaryOperator::Or) {
            // Short-circuits: errors in an operand that is not needed are not reported.
            let want = operator == BinaryOperator::Or;
            let lhs = self.eval_bool(&op.lhs_expr, operator, scope)?;
            if lhs == want {
                return Ok(Value::Bool(want));
            }
            return self
                .eval_bool(&op.rhs_expr, operator, scope)
                .map(Value::Bool);
        }
        let lhs = self.eval(&op.lhs_expr, scope);
        let rhs = self.eval(&op.rhs_expr, scope);
        let (lhs, rhs) = (lhs?, rhs?);
        let symbol = operator.as_str();
        let fail = |message: String| Err(self.error(Diagnostic::error(message).at(at)));
        match operator {
            BinaryOperator::Eq | BinaryOperator::NotEq => match equal(&lhs, &rhs) {
                Ok(eq) => Ok(Value::Bool(eq == (operator == BinaryOperator::Eq))),
                Err(message) => fail(format!("cannot compare with `{symbol}`: {message}")),
            },
            _ => {
                let (Value::Number(a), Value::Number(b)) = (&lhs, &rhs) else {
                    return fail(format!(
                        "`{symbol}` needs numbers, found {} and {}",
                        lhs.type_name(),
                        rhs.type_name()
                    ));
                };
                match arithmetic(operator, a, b) {
                    Ok(v) => Ok(v),
                    Err(message) => fail(message),
                }
            }
        }
    }

    fn eval_bool(
        &self,
        expr: &Expression,
        operator: BinaryOperator,
        scope: &Scope<'_>,
    ) -> Eval<bool> {
        match self.eval(expr, scope)? {
            Value::Bool(b) => Ok(b),
            other => Err(self.error(
                Diagnostic::error(format!(
                    "`{}` needs bools, found {}",
                    operator.as_str(),
                    other.type_name()
                ))
                .at(scope.span(expr)),
            )),
        }
    }

    fn for_expr(&self, f: &ForExpr, scope: &Scope<'_>) -> Eval {
        let intro = &f.intro;
        let collection = self.eval(&intro.collection_expr, scope)?;
        let items = iteration_items(collection).map_err(|other| {
            self.error(
                Diagnostic::error(format!(
                    "a `for` expression needs a list or an object, found {}",
                    other.type_name()
                ))
                .at(scope.span(&intro.collection_expr)),
            )
        })?;
        let key_var = intro.key_var.as_ref().map(|k| k.as_str());
        let value_var = intro.value_var.as_str();
        let mut list = Vec::new();
        let mut object: IndexMap<String, Value> = IndexMap::new();
        let mut groups: IndexMap<String, Vec<Value>> = IndexMap::new();
        for (key, value) in items {
            with_bindings(scope, key_var, key, value_var, value, |inner| {
                if let Some(cond) = &f.cond {
                    match self.eval(&cond.expr, inner)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => return Ok(()),
                        other => {
                            return Err(self.error(
                                Diagnostic::error(format!(
                                    "the `if` clause must be a bool, found {}",
                                    other.type_name()
                                ))
                                .at(inner.span(&cond.expr)),
                            ));
                        }
                    }
                }
                let Some(key_expr) = &f.key_expr else {
                    list.push(self.eval(&f.value_expr, inner)?);
                    return Ok(());
                };
                let key = self.object_key(key_expr, inner);
                let value = self.eval(&f.value_expr, inner);
                let (key, value) = (key?, value?);
                if f.grouping {
                    groups.entry(key).or_default().push(value);
                } else if object.contains_key(&key) {
                    return Err(self.error(
                        Diagnostic::error(format!("duplicate key `{key}` in `for` expression"))
                            .at(inner.span(key_expr))
                            .with_help("add `...` after the value to group values by key"),
                    ));
                } else {
                    object.insert(key, value);
                }
                Ok(())
            })?;
        }
        Ok(match (&f.key_expr, f.grouping) {
            (None, _) => Value::List(list),
            (Some(_), false) => Value::Object(object),
            (Some(_), true) => Value::Object(
                groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::List(v)))
                    .collect(),
            ),
        })
    }

    fn call(&self, call: &FuncCall, scope: &Scope<'_>) -> Eval {
        let at = scope.span(call);
        let name = call.name.name.as_str();
        if call.name.is_namespaced() {
            return Err(
                self.error(Diagnostic::error("namespaced functions are not supported").at(at))
            );
        }
        let builtin = Builtin::lookup(name);
        if builtin == Some(Builtin::Try) {
            return self.call_try(call, scope, at);
        }
        let mut args = Vec::new();
        let mut failed = false;
        for expr in call.args.iter() {
            let span = match expr.span() {
                Some(r) => span(scope.file, Some(r)),
                None => at,
            };
            match self.eval(expr, scope) {
                Ok(value) => args.push(Arg { value, span }),
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(Fail);
        }
        if call.args.expand_final() {
            if builtin.is_none() {
                return Err(self.error(
                    Diagnostic::error(format!(
                        "`...` expansion is only supported for HCL functions, not `{name}`"
                    ))
                    .at(at),
                ));
            }
            if let Some(last) = args.pop() {
                let Value::List(items) = last.value else {
                    return Err(self.error(
                        Diagnostic::error(format!(
                            "`...` needs a list, found {}",
                            last.value.type_name()
                        ))
                        .at(last.span),
                    ));
                };
                args.extend(items.into_iter().map(|value| Arg {
                    value,
                    span: last.span,
                }));
            }
        }
        match builtin {
            Some(builtin) => self.call_builtin(builtin, args, at, scope),
            None => Ok(Value::Call {
                name: name.to_owned(),
                args: args.into_iter().map(|a| a.value).collect(),
            }),
        }
    }

    fn call_builtin(&self, builtin: Builtin, args: Vec<Arg>, at: Span, scope: &Scope<'_>) -> Eval {
        let site = CallSite {
            span: at,
            base_dir: self.sources.path(scope.file).parent(),
        };
        functions::call(builtin, args, &site).map_err(|d| self.error(d))
    }

    /// `try(a, b, ...)`: the first argument that evaluates without errors. Diagnostics of failed
    /// attempts are discarded.
    fn call_try(&self, call: &FuncCall, scope: &Scope<'_>, at: Span) -> Eval {
        let mut last_error = None;
        for expr in call.args.iter() {
            let mark = self.diags.borrow().len();
            if let Ok(value) = self.eval(expr, scope) {
                return Ok(value);
            }
            let discarded: Vec<Diagnostic> = self.diags.borrow_mut().drain(mark..).collect();
            last_error = discarded.into_iter().next().map(|d| d.message);
        }
        let mut d = Diagnostic::error(if call.args.is_empty() {
            "`try` needs at least one argument"
        } else {
            "no argument of `try` could be evaluated"
        })
        .at(at);
        if let Some(message) = last_error {
            d = d.with_help(format!("the last argument failed with: {message}"));
        }
        Err(self.error(d))
    }
}

/// One step of a traversal after its root: `.name`, `[expr]` or `.0`.
pub(super) struct Segment {
    pub key: Key,
    pub span: Span,
}

pub(super) enum Key {
    Name(String),
    Value(Value),
}

impl Key {
    /// The segment as a block label: identifiers, strings and numbers.
    pub fn as_label(&self) -> Option<String> {
        match self {
            Key::Name(s) | Key::Value(Value::String(s)) => Some(s.clone()),
            Key::Value(Value::Number(n)) => Some(n.to_string()),
            Key::Value(_) => None,
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Name(name) => write!(f, ".{name}"),
            Key::Value(Value::String(s)) => write!(f, "[{}]", crate::write::quote(s)),
            Key::Value(Value::Number(n)) => write!(f, "[{n}]"),
            Key::Value(other) => write!(f, "[<{}>]", other.type_name()),
        }
    }
}

fn negate(n: Number) -> Number {
    match n {
        Number::Int(i) => match i.checked_neg() {
            Some(i) => Number::Int(i),
            None => Number::Text(format!("{}", -(i as i128))),
        },
        Number::Float(f) => Number::Float(-f),
        Number::Text(t) => match t.strip_prefix('-') {
            Some(rest) => Number::from_literal(rest).unwrap_or(Number::Text(rest.to_owned())),
            None => Number::Text(format!("-{t}")),
        },
    }
}

fn arithmetic(operator: BinaryOperator, a: &Number, b: &Number) -> Result<Value, String> {
    use BinaryOperator as Op;
    if let (Number::Int(x), Number::Int(y)) = (a, b) {
        let (x, y) = (*x, *y);
        let exact = match operator {
            Op::Plus => x.checked_add(y),
            Op::Minus => x.checked_sub(y),
            Op::Mul => x.checked_mul(y),
            Op::Div if y == 0 => return Err("division by zero".to_owned()),
            Op::Div if x % y == 0 => x.checked_div(y),
            Op::Mod if y == 0 => return Err("modulo by zero".to_owned()),
            Op::Mod => x.checked_rem(y),
            Op::Less => return Ok(Value::Bool(x < y)),
            Op::LessEq => return Ok(Value::Bool(x <= y)),
            Op::Greater => return Ok(Value::Bool(x > y)),
            Op::GreaterEq => return Ok(Value::Bool(x >= y)),
            _ => None,
        };
        if let Some(n) = exact {
            return Ok(Value::Number(Number::Int(n)));
        }
    }
    let (x, y) = (a.as_f64(), b.as_f64());
    let result = match operator {
        Op::Plus => x + y,
        Op::Minus => x - y,
        Op::Mul => x * y,
        Op::Div if y == 0.0 => return Err("division by zero".to_owned()),
        Op::Div => x / y,
        Op::Mod if y == 0.0 => return Err("modulo by zero".to_owned()),
        Op::Mod => x % y,
        Op::Less => return Ok(Value::Bool(x < y)),
        Op::LessEq => return Ok(Value::Bool(x <= y)),
        Op::Greater => return Ok(Value::Bool(x > y)),
        Op::GreaterEq => return Ok(Value::Bool(x >= y)),
        _ => unreachable!("not an arithmetic operator: {operator:?}"),
    };
    Number::from_f64(result)
        .map(Value::Number)
        .ok_or_else(|| "the result is not a finite number".to_owned())
}

/// Equality for `==` and `!=`. References compare with references and identifiers with
/// identifiers; calls and `sql()` values cannot be compared.
pub(super) fn equal(a: &Value, b: &Value) -> Result<bool, String> {
    match (a, b) {
        (Value::Ref(x), Value::Ref(y)) => Ok(x == y),
        (Value::Ident(x), Value::Ident(y)) => Ok(x == y),
        (Value::Ref(_) | Value::Ident(_) | Value::Call { .. } | Value::Sql(_), _)
        | (_, Value::Ref(_) | Value::Ident(_) | Value::Call { .. } | Value::Sql(_)) => {
            Err(format!("{} and {}", a.type_name(), b.type_name()))
        }
        (Value::Number(x), Value::Number(y)) => Ok(match (x, y) {
            (Number::Int(x), Number::Int(y)) => x == y,
            (Number::Text(x), Number::Text(y)) if x == y => true,
            _ => x.as_f64() == y.as_f64(),
        }),
        (Value::List(x), Value::List(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (x, y) in x.iter().zip(y) {
                if !equal(x, y)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (k, x) in x {
                match y.get(k) {
                    Some(y) if equal(x, y)? => {}
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
        _ => Ok(a == b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: i64) -> Number {
        Number::Int(i)
    }

    #[test]
    fn integer_arithmetic_stays_exact() {
        assert_eq!(
            arithmetic(BinaryOperator::Plus, &n(2), &n(3)),
            Ok(Value::Number(n(5)))
        );
        assert_eq!(
            arithmetic(BinaryOperator::Div, &n(6), &n(3)),
            Ok(Value::Number(n(2)))
        );
        assert_eq!(
            arithmetic(BinaryOperator::Div, &n(7), &n(2)),
            Ok(Value::Number(Number::Float(3.5)))
        );
        assert_eq!(
            arithmetic(BinaryOperator::Mod, &n(7), &n(3)),
            Ok(Value::Number(n(1)))
        );
        assert!(arithmetic(BinaryOperator::Div, &n(1), &n(0)).is_err());
        assert!(arithmetic(BinaryOperator::Mod, &n(1), &n(0)).is_err());
        assert_eq!(
            arithmetic(BinaryOperator::Plus, &n(i64::MAX), &n(1)),
            Ok(Value::Number(Number::Float(9223372036854775808.0)))
        );
    }

    #[test]
    fn mixed_arithmetic_uses_floats() {
        assert_eq!(
            arithmetic(BinaryOperator::Mul, &Number::Float(0.5), &n(4)),
            Ok(Value::Number(n(2)))
        );
        assert_eq!(
            arithmetic(BinaryOperator::Less, &Number::Text("1.50".into()), &n(2)),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn negation() {
        assert_eq!(negate(n(5)), n(-5));
        assert_eq!(
            negate(n(i64::MIN)),
            Number::Text("9223372036854775808".into())
        );
        assert_eq!(
            negate(Number::Text("1.50".into())),
            Number::Text("-1.50".into())
        );
        assert_eq!(
            negate(Number::Text("-1.50".into())),
            Number::Text("1.50".into())
        );
    }

    #[test]
    fn equality_rules() {
        let r = |l: &str| Value::Ref(crate::Address::new("role", vec![l.to_owned()]));
        assert_eq!(equal(&r("a"), &r("a")), Ok(true));
        assert_eq!(equal(&r("a"), &r("b")), Ok(false));
        assert_eq!(
            equal(&Value::Ident("x".into()), &Value::Ident("x".into())),
            Ok(true)
        );
        assert!(equal(&r("a"), &Value::String("a".into())).is_err());
        assert!(equal(&Value::Sql("x".into()), &Value::Sql("x".into())).is_err());
        assert_eq!(
            equal(&Value::Number(n(1)), &Value::Number(Number::Float(1.0))),
            Ok(true)
        );
        assert_eq!(
            equal(&Value::Number(n(1)), &Value::String("1".into())),
            Ok(false)
        );
        assert_eq!(equal(&Value::Null, &Value::Null), Ok(true));
        assert_eq!(
            equal(
                &Value::List(vec![Value::Bool(true)]),
                &Value::List(vec![Value::Bool(true)])
            ),
            Ok(true)
        );
    }
}
