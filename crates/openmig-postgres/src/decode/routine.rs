//! `function` and `procedure` blocks.

use super::Decoder;
use super::keywords::Security;
use super::types::{TypeContext, default_sql};
use crate::model::{
    ArgMode, Function, FunctionArg, FunctionBody, Parallel, ReturnColumn, Returns, Volatility,
    function_signature,
};
use openmig_core::{ObjectKind, ObjectRef};
use openmig_hcl::{Block, BlockDecoder, Number, Value};

/// Whether a `sql` body is SQL-standard (`BEGIN ATOMIC ... END` or `RETURN ...`), which
/// PostgreSQL parses at creation, rather than a string definition.
pub(crate) fn is_standard_body(language: &str, body: &str) -> bool {
    if language != "sql" {
        return false;
    }
    let words: Vec<String> = body
        .split_whitespace()
        .take(2)
        .map(str::to_ascii_uppercase)
        .collect();
    match words.as_slice() {
        [first, ..] if first == "RETURN" => true,
        [first, second] => first == "BEGIN" && second == "ATOMIC",
        _ => false,
    }
}

/// PostgreSQL's default `COST`: 1 for C and internal functions, 100 otherwise.
pub(crate) fn default_cost(language: &str) -> &'static str {
    match language {
        "c" | "internal" => "1",
        _ => "100",
    }
}

/// PostgreSQL's default `ROWS`: 1000 for set-returning functions, 0 otherwise.
pub(crate) fn default_rows(returns: Option<&Returns>) -> &'static str {
    match returns {
        Some(Returns::Table(_) | Returns::Type { setof: true, .. }) => "1000",
        _ => "0",
    }
}

/// The result type PostgreSQL infers from output arguments: the type of a single one, `record`
/// for several.
pub(crate) fn implied_return(args: &[FunctionArg]) -> Option<String> {
    let outputs: Vec<&FunctionArg> = args
        .iter()
        .filter(|a| matches!(a.mode, ArgMode::Out | ArgMode::InOut))
        .collect();
    match outputs.as_slice() {
        [] => None,
        [one] => Some(one.data_type.clone()),
        _ => Some("record".to_owned()),
    }
}

impl<'a> Decoder<'a> {
    pub(super) fn routine(&mut self, block: &'a Block, procedure: bool) {
        let mut dec = self.decoder(block);
        let qn = self.qualified_name(&mut dec);
        let (kind, object) = if procedure {
            (ObjectKind::Procedure, "procedure")
        } else {
            (ObjectKind::Function, "function")
        };
        let mut ok = true;
        let mut args = Vec::new();
        for mut a in dec.children("arg") {
            match self.arg(&mut a) {
                Some(arg) => args.push(arg),
                None => ok = false,
            }
            a.finish();
        }
        let returns = if procedure {
            None
        } else {
            self.returns(&mut dec, &args)
        };
        let language = match dec.value("language") {
            Some(Value::Ident(w) | Value::String(w)) if !w.is_empty() => w.to_ascii_lowercase(),
            Some(other) => {
                self.error(
                    block.header_span,
                    format!(
                        "`language` must be a language name, found {}",
                        other.type_name()
                    ),
                );
                String::new()
            }
            None => {
                self.error(
                    block.header_span,
                    format!("`{object}` requires the attribute `language`"),
                );
                String::new()
            }
        };
        let body = dec.req::<String>("body").unwrap_or_default();
        let body = if is_standard_body(&language, &body) {
            FunctionBody::Standard(body)
        } else {
            FunctionBody::Definition(body)
        };
        let security: Option<Security> = dec.keyword("security");
        let (volatility, strict, leakproof, parallel, window) = if procedure {
            (Volatility::Volatile, false, false, Parallel::Unsafe, false)
        } else {
            (
                dec.keyword("volatility").unwrap_or_default(),
                dec.opt("strict").unwrap_or(false),
                dec.opt("leakproof").unwrap_or(false),
                dec.keyword("parallel").unwrap_or_default(),
                dec.opt("window").unwrap_or(false),
            )
        };
        let cost = if procedure {
            None
        } else {
            dec.opt::<Number>("cost").map(|n| n.to_string())
        };
        let rows = if procedure {
            None
        } else {
            dec.opt::<Number>("rows").map(|n| n.to_string())
        };
        let config = self.options(&mut dec, "set");
        let comment = self.comment(&mut dec);
        let input_types: Vec<String> = args
            .iter()
            .filter(|a| a.mode.is_input())
            .map(|a| a.data_type.clone())
            .collect();
        let signature = function_signature(&qn.name, input_types.iter().map(String::as_str));
        let r = ObjectRef::new(kind, [&qn.schema, &signature]);
        self.depends_on(&mut dec, &r);
        dec.finish();
        if !ok || (!procedure && returns.is_none()) || language.is_empty() {
            return;
        }
        if !self.declare(r, block.header_span, block.each_key.is_some()) {
            return;
        }
        let function = Function {
            name: qn.name.clone(),
            cost: cost.unwrap_or_else(|| default_cost(&language).to_owned()),
            rows: rows.unwrap_or_else(|| default_rows(returns.as_ref()).to_owned()),
            args,
            returns,
            language,
            body,
            volatility,
            strict,
            security_definer: security == Some(Security::Definer),
            leakproof,
            parallel,
            window,
            config,
            comment,
            owner: String::new(),
            acl: self.default_acl(kind),
        };
        let schema = self.schema_mut(&qn.schema, block.header_span);
        let routines = if procedure {
            &mut schema.procedures
        } else {
            &mut schema.functions
        };
        routines.insert(signature, function);
    }

    fn arg(&mut self, dec: &mut BlockDecoder<'_>) -> Option<FunctionArg> {
        let name = dec
            .opt::<String>("name")
            .or_else(|| dec.block().labels.first().cloned())
            .filter(|n| !n.is_empty());
        let mode: ArgMode = dec.keyword("mode").unwrap_or_default();
        let data_type = self.type_attr(dec, TypeContext::Routine)?;
        let default = match dec.attr("default") {
            Some(attr) => match default_sql(&attr.value, &data_type) {
                Ok(text) => text,
                Err(message) => {
                    self.error(attr.value_span, message);
                    return None;
                }
            },
            None => None,
        };
        Some(FunctionArg {
            name,
            mode,
            data_type,
            default,
        })
    }

    fn returns(&mut self, dec: &mut BlockDecoder<'_>, args: &[FunctionArg]) -> Option<Returns> {
        let span = dec.block().header_span;
        let setof: bool = dec.opt("setof").unwrap_or(false);
        let mut tables = dec.children("returns_table");
        let declared = dec
            .attr("returns")
            .filter(|a| !matches!(a.value, Value::Null));
        if tables.len() > 1 {
            self.error(
                tables[1].block().header_span,
                "only one `returns_table` is allowed",
            );
        }
        if let Some(mut table) = tables.drain(..).next() {
            if declared.is_some() || setof {
                self.error(
                    span,
                    "`returns_table` cannot be combined with `returns` or `setof`",
                );
            }
            let mut columns = Vec::new();
            for mut c in table.children("column") {
                let name = self.nested_name(&mut c);
                let data_type = self.type_attr(&mut c, TypeContext::Routine);
                if let (Some(name), Some(data_type)) = (name, data_type) {
                    columns.push(ReturnColumn { name, data_type });
                }
                c.finish();
            }
            table.finish();
            if columns.is_empty() {
                self.error(span, "`returns_table` needs at least one `column`");
                return None;
            }
            return Some(Returns::Table(columns));
        }
        let data_type = match declared {
            Some(attr) => match self.type_text(&attr.value, TypeContext::Routine) {
                Ok(t) => t,
                Err(message) => {
                    self.error(attr.value_span, message);
                    return None;
                }
            },
            None => match implied_return(args) {
                Some(t) => t,
                None => {
                    self.error(
                        span,
                        "`function` requires `returns` or `returns_table` (or OUT arguments)",
                    );
                    return None;
                }
            },
        };
        Some(Returns::Type { data_type, setof })
    }
}
