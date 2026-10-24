//! String templates and heredocs: interpolation, `%{ if }` / `%{ for }` directives and `~` strip
//! markers.

use super::Loader;
use super::expr::{Eval, Fail, Scope, iteration_items, with_bindings};
use crate::diag::Diagnostic;
use crate::source::Span;
use crate::value::Value;
use hcl_edit::template::{Directive, Element, Template};

impl Loader<'_> {
    pub(super) fn eval_template(&self, template: &Template, scope: &Scope<'_>) -> Eval<String> {
        let mut out = String::new();
        self.render(template, scope, false, false, &mut out)?;
        Ok(out)
    }

    /// Renders `template` into `out`. `strip_first` / `strip_last` come from strip markers of the
    /// directive tags around a nested template.
    fn render(
        &self,
        template: &Template,
        scope: &Scope<'_>,
        strip_first: bool,
        strip_last: bool,
        out: &mut String,
    ) -> Eval<()> {
        let elements: Vec<&Element> = template.iter().collect();
        let mut failed = false;
        for (i, element) in elements.iter().enumerate() {
            match element {
                Element::Literal(literal) => {
                    let mut text = literal.value().as_str();
                    let after_marker = match i.checked_sub(1) {
                        Some(prev) => strips_following(elements[prev]),
                        None => strip_first,
                    };
                    let before_marker = match elements.get(i + 1) {
                        Some(next) => strips_preceding(next),
                        None => strip_last,
                    };
                    if after_marker {
                        text = text.trim_start();
                    }
                    if before_marker {
                        text = text.trim_end();
                    }
                    out.push_str(text);
                }
                Element::Interpolation(interpolation) => {
                    let expr = &interpolation.expr;
                    let at = scope.span(expr);
                    match self
                        .eval(expr, scope)
                        .and_then(|value| self.interpolate(value, at))
                    {
                        Ok(text) => out.push_str(&text),
                        Err(_) => failed = true,
                    }
                }
                Element::Directive(directive) => {
                    if self.directive(directive, scope, out).is_err() {
                        failed = true;
                    }
                }
            }
        }
        if failed { Err(Fail) } else { Ok(()) }
    }

    fn directive(&self, directive: &Directive, scope: &Scope<'_>, out: &mut String) -> Eval<()> {
        match directive {
            Directive::If(d) => {
                let cond = &d.if_expr.cond_expr;
                let taken = match self.eval(cond, scope)? {
                    Value::Bool(b) => b,
                    other => {
                        return Err(self.error(
                            Diagnostic::error(format!(
                                "the `if` condition must be a bool, found {}",
                                other.type_name()
                            ))
                            .at(scope.span(cond)),
                        ));
                    }
                };
                let endif_strip = d.endif_expr.strip.strip_start();
                if taken {
                    let strip_last = d
                        .else_expr
                        .as_ref()
                        .map_or(endif_strip, |e| e.strip.strip_start());
                    let strip_first = d.if_expr.strip.strip_end();
                    self.render(&d.if_expr.template, scope, strip_first, strip_last, out)
                } else if let Some(e) = &d.else_expr {
                    self.render(&e.template, scope, e.strip.strip_end(), endif_strip, out)
                } else {
                    Ok(())
                }
            }
            Directive::For(d) => {
                let f = &d.for_expr;
                let collection = self.eval(&f.collection_expr, scope)?;
                let items = iteration_items(collection).map_err(|other| {
                    self.error(
                        Diagnostic::error(format!(
                            "a `for` directive needs a list or an object, found {}",
                            other.type_name()
                        ))
                        .at(scope.span(&f.collection_expr)),
                    )
                })?;
                let key_var = f.key_var.as_ref().map(|k| k.as_str());
                let strip_first = f.strip.strip_end();
                let strip_last = d.endfor_expr.strip.strip_start();
                for (key, value) in items {
                    with_bindings(scope, key_var, key, f.value_var.as_str(), value, |inner| {
                        self.render(&f.template, inner, strip_first, strip_last, out)
                    })?;
                }
                Ok(())
            }
        }
    }

    /// The text of an interpolated value.
    fn interpolate(&self, value: Value, at: Span) -> Eval<String> {
        let (message, help) = match value {
            Value::String(s) => return Ok(s),
            Value::Number(n) => return Ok(n.to_string()),
            Value::Bool(b) => return Ok(b.to_string()),
            Value::Null => ("cannot interpolate null".to_owned(), None),
            Value::Ref(address) => (
                format!("cannot interpolate the reference `{address}`"),
                Some(format!(
                    "use `{address}.name` for the name of the object in the database"
                )),
            ),
            Value::Ident(word) => (
                format!("cannot interpolate the identifier `{word}`"),
                Some(format!(
                    "`{word}` is not a variable or a reference; write it as text: \"{word}\""
                )),
            ),
            Value::Call { name, .. } => (
                format!("cannot interpolate the call `{name}(...)`"),
                Some("only HCL functions such as `format` or `join` produce text".to_owned()),
            ),
            Value::Sql(_) => ("cannot interpolate a `sql()` value".to_owned(), None),
            Value::List(_) | Value::Object(_) => (
                format!(
                    "cannot interpolate {}",
                    if matches!(value, Value::List(_)) {
                        "a list"
                    } else {
                        "an object"
                    }
                ),
                Some("convert it to text with `join()` or `jsonencode()`".to_owned()),
            ),
        };
        let mut d = Diagnostic::error(message).at(at);
        if let Some(help) = help {
            d = d.with_help(help);
        }
        Err(self.error(d))
    }
}

/// Whether the element's closing tag strips whitespace from the literal that follows it.
fn strips_following(element: &Element) -> bool {
    match element {
        Element::Literal(_) => false,
        Element::Interpolation(i) => i.strip.strip_end(),
        Element::Directive(d) => match d.as_ref() {
            Directive::If(d) => d.endif_expr.strip.strip_end(),
            Directive::For(d) => d.endfor_expr.strip.strip_end(),
        },
    }
}

/// Whether the element's opening tag strips whitespace from the literal before it.
fn strips_preceding(element: &Element) -> bool {
    match element {
        Element::Literal(_) => false,
        Element::Interpolation(i) => i.strip.strip_start(),
        Element::Directive(d) => match d.as_ref() {
            Directive::If(d) => d.if_expr.strip.strip_start(),
            Directive::For(d) => d.for_expr.strip.strip_start(),
        },
    }
}
