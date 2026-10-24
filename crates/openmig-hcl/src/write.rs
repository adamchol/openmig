//! Canonical HCL output, used by `openmig inspect` to turn a live database into schema files.
//!
//! Formatting rules: two-space indentation, `=` aligned within a run of consecutive single-line
//! attributes, one blank line between blocks, multi-line strings written as `<<-SQL` heredocs
//! indented with the block, strings that contain `${` or `%{` escaped as `$${` / `%%{`.
//!
//! Every value survives a round trip through the loader exactly:
//!
//! * A heredoc always ends with a newline, so a multi-line string without a trailing newline is
//!   written as `chomp(<<-SQL ... SQL)`.
//! * `<<-` strips the indentation common to all lines, so a string whose lines all start with
//!   whitespace is written as a plain `<<SQL` heredoc with its lines verbatim.
//! * Strings containing `\r` or other control characters (except tabs) are written quoted, with
//!   escapes, even when they span lines.
//! * Identifiers that are not valid bare HCL identifiers (or are keywords such as `null`) are
//!   written as quoted strings, which engines accept wherever keywords and types are expected.
//! * Integer literals above `u64::MAX`, which the parser rejects, get a `.0` suffix.
//!
//! Attribute names, block kinds and call names are written verbatim and must be identifiers; call
//! names must not be HCL function names (`format`, `range`, ...), which the loader would evaluate.

use crate::value::Address;
use hcl_primitives::ident::{is_id_continue, is_ident};
use std::fmt::Write as _;

/// An expression to write.
#[derive(Clone, Debug, PartialEq)]
pub enum WExpr {
    Null,
    Bool(bool),
    /// Number text written verbatim.
    Number(String),
    /// A quoted string (escaped as needed). Written as a heredoc when it contains newlines.
    String(String),
    /// A bare identifier: `text`, `CASCADE`.
    Ident(String),
    /// A function-call shaped value: `varchar(255)`, `ZSTD(1)`.
    Call(String, Vec<WExpr>),
    /// A reference: `table.app.users.column.id`.
    Ref(Address),
    /// `sql("...")`.
    Sql(String),
    List(Vec<WExpr>),
    /// An object; keys are written bare when they are identifiers, quoted otherwise.
    Object(Vec<(String, WExpr)>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WItem {
    Attr(String, WExpr),
    Block(WBlock),
    /// A `#` comment line.
    Comment(String),
    /// An explicit blank line.
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WBlock {
    pub kind: String,
    pub labels: Vec<String>,
    pub items: Vec<WItem>,
}

impl WBlock {
    pub fn new(kind: impl Into<String>, labels: Vec<String>) -> Self {
        Self {
            kind: kind.into(),
            labels,
            items: Vec::new(),
        }
    }

    pub fn attr(mut self, name: impl Into<String>, value: WExpr) -> Self {
        self.items.push(WItem::Attr(name.into(), value));
        self
    }

    pub fn block(mut self, block: WBlock) -> Self {
        self.items.push(WItem::Block(block));
        self
    }

    pub fn push_attr(&mut self, name: impl Into<String>, value: WExpr) {
        self.items.push(WItem::Attr(name.into(), value));
    }

    pub fn push_block(&mut self, block: WBlock) {
        self.items.push(WItem::Block(block));
    }
}

/// Render top-level blocks as one HCL file.
pub fn render(blocks: &[WBlock]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        write_block(&mut out, block, 0);
    }
    out
}

/// Render a single expression as it would be written at the top level (for messages and tests).
pub fn render_expr(expr: &WExpr) -> String {
    expr_text(expr, 0, 0)
}

const INDENT: &str = "  ";
const MAX_WIDTH: usize = 100;

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str(INDENT);
    }
}

fn write_block(out: &mut String, block: &WBlock, level: usize) {
    indent(out, level);
    out.push_str(&block.kind);
    for label in &block.labels {
        out.push(' ');
        out.push_str(&quote(label));
    }
    if block.items.iter().all(|i| matches!(i, WItem::Blank)) {
        out.push_str(" {}\n");
        return;
    }
    out.push_str(" {\n");
    write_body(out, &block.items, level + 1);
    indent(out, level);
    out.push_str("}\n");
}

/// Blank lines between body items are requested rather than written: requests collapse, and
/// requests at the start or end of a body are dropped.
#[derive(Default)]
struct Spacing {
    started: bool,
    blank: bool,
}

impl Spacing {
    fn request_blank(&mut self) {
        self.blank = true;
    }

    fn before_item(&mut self, out: &mut String) {
        if self.blank && self.started {
            out.push('\n');
        }
        self.blank = false;
        self.started = true;
    }
}

fn write_body(out: &mut String, items: &[WItem], level: usize) {
    let mut spacing = Spacing::default();
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            WItem::Blank => {
                spacing.request_blank();
                i += 1;
            }
            WItem::Comment(text) => {
                spacing.before_item(out);
                for line in text.split('\n') {
                    indent(out, level);
                    out.push('#');
                    if !line.is_empty() {
                        out.push(' ');
                        out.push_str(line);
                    }
                    out.push('\n');
                }
                i += 1;
            }
            WItem::Block(block) => {
                spacing.request_blank();
                spacing.before_item(out);
                write_block(out, block, level);
                spacing.request_blank();
                i += 1;
            }
            WItem::Attr(..) => {
                spacing.before_item(out);
                let start = i;
                while i < items.len() && matches!(items[i], WItem::Attr(..)) {
                    i += 1;
                }
                let attrs = items[start..i].iter().filter_map(|item| match item {
                    WItem::Attr(name, value) => Some((name.as_str(), value)),
                    _ => None,
                });
                write_assignments(out, attrs, level);
            }
        }
    }
}

/// Writes `name = value` lines, aligning `=` within runs of consecutive single-line values.
fn write_assignments<'e>(
    out: &mut String,
    entries: impl Iterator<Item = (&'e str, &'e WExpr)>,
    level: usize,
) {
    let rendered: Vec<(&str, String)> = entries
        .map(|(name, value)| {
            let col = level * INDENT.len() + width(name) + 3;
            (name, expr_text(value, level, col))
        })
        .collect();
    let mut i = 0;
    while i < rendered.len() {
        let single = |v: &String| !v.contains('\n');
        let end = if single(&rendered[i].1) {
            i + rendered[i..].iter().take_while(|(_, v)| single(v)).count()
        } else {
            i + 1
        };
        let pad = rendered[i..end]
            .iter()
            .map(|(name, _)| width(name))
            .max()
            .unwrap_or(0);
        for (name, value) in &rendered[i..end] {
            indent(out, level);
            out.push_str(name);
            for _ in width(name)..pad {
                out.push(' ');
            }
            out.push_str(" = ");
            out.push_str(value);
            out.push('\n');
        }
        i = end;
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// Column after appending `text` at column `col`.
fn advance(col: usize, text: &str) -> usize {
    match text.rfind('\n') {
        Some(nl) => width(&text[nl + 1..]),
        None => col + width(text),
    }
}

/// Renders `expr` starting at column `col` on a line indented at `level`.
fn expr_text(expr: &WExpr, level: usize, col: usize) -> String {
    match expr {
        WExpr::List(items) if !items.is_empty() => {
            if let Some(line) = inline(expr).filter(|l| col + width(l) <= MAX_WIDTH) {
                return line;
            }
            let mut out = String::from("[\n");
            for item in items {
                indent(&mut out, level + 1);
                out.push_str(&expr_text(item, level + 1, (level + 1) * INDENT.len()));
                out.push_str(",\n");
            }
            indent(&mut out, level);
            out.push(']');
            out
        }
        WExpr::Object(entries) if !entries.is_empty() => {
            if let Some(line) = inline(expr).filter(|l| col + width(l) <= MAX_WIDTH) {
                return line;
            }
            let keys: Vec<String> = entries.iter().map(|(k, _)| object_key(k)).collect();
            let mut out = String::from("{\n");
            write_assignments(
                &mut out,
                keys.iter()
                    .map(String::as_str)
                    .zip(entries.iter().map(|(_, v)| v)),
                level + 1,
            );
            indent(&mut out, level);
            out.push('}');
            out
        }
        WExpr::Call(name, args) => {
            let mut out = format!("{name}(");
            let mut col = col + width(name) + 1;
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                    col += 2;
                }
                let text = expr_text(arg, level, col);
                col = advance(col, &text);
                out.push_str(&text);
            }
            out.push(')');
            out
        }
        WExpr::String(s) => string_text(s, level),
        WExpr::Sql(s) => format!("sql({})", string_text(s, level)),
        _ => inline(expr).unwrap_or_default(),
    }
}

/// The single-line form, or `None` when the expression contains a heredoc.
fn inline(expr: &WExpr) -> Option<String> {
    Some(match expr {
        WExpr::Null => "null".to_owned(),
        WExpr::Bool(b) => b.to_string(),
        WExpr::Number(text) => number_text(text),
        WExpr::String(s) if !needs_heredoc(s) => quote(s),
        WExpr::Sql(s) if !needs_heredoc(s) => format!("sql({})", quote(s)),
        WExpr::String(_) | WExpr::Sql(_) => return None,
        WExpr::Ident(word) if is_bare(word) => word.clone(),
        WExpr::Ident(word) => quote(word),
        WExpr::Ref(address) => address.to_string(),
        WExpr::Call(name, args) => format!("{name}({})", inline_all(args, ", ")?),
        WExpr::List(items) => format!("[{}]", inline_all(items, ", ")?),
        WExpr::Object(entries) if entries.is_empty() => "{}".to_owned(),
        WExpr::Object(entries) => {
            let parts = entries
                .iter()
                .map(|(k, v)| Some(format!("{} = {}", object_key(k), inline(v)?)))
                .collect::<Option<Vec<_>>>()?;
            format!("{{ {} }}", parts.join(", "))
        }
    })
}

fn inline_all(items: &[WExpr], sep: &str) -> Option<String> {
    Some(
        items
            .iter()
            .map(inline)
            .collect::<Option<Vec<_>>>()?
            .join(sep),
    )
}

/// Words that cannot be written bare: literals, `for` expression keywords and the roots the
/// evaluator reserves.
const RESERVED: &[&str] = &[
    "true", "false", "null", "for", "in", "if", "var", "local", "each",
];

fn is_bare(word: &str) -> bool {
    is_ident(word) && !RESERVED.contains(&word)
}

fn object_key(key: &str) -> String {
    if is_bare(key) {
        key.to_owned()
    } else {
        quote(key)
    }
}

/// hcl-edit cannot parse integer literals above `u64::MAX`; `.0` keeps such numbers loadable.
fn number_text(text: &str) -> String {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits.parse::<u64>().is_err()
    {
        format!("{text}.0")
    } else {
        text.to_owned()
    }
}

/// A quoted HCL string literal with escapes, including `$${` and `%%{` for template markers.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' | '%' if chars.peek() == Some(&'{') => {
                out.push(c);
                out.push(c);
            }
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn needs_heredoc(s: &str) -> bool {
    s.contains('\n') && !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

fn string_text(s: &str, level: usize) -> String {
    if needs_heredoc(s) {
        heredoc(s, level)
    } else {
        quote(s)
    }
}

fn heredoc(s: &str, level: usize) -> String {
    let (content, chomp) = match s.strip_suffix('\n') {
        Some(content) => (content, false),
        None => (s, true),
    };
    let lines: Vec<&str> = content.split('\n').collect();
    let delimiter = delimiter(&lines);
    // `<<-` removes the smallest indentation of the non-empty lines (counting `char::is_whitespace`
    // like hcl-edit does), which must be exactly the indentation added here: some line must have
    // none of its own.
    let mut non_empty = lines.iter().filter(|l| !l.is_empty()).peekable();
    let indented =
        non_empty.peek().is_none() || non_empty.any(|l| !l.starts_with(char::is_whitespace));
    let mut out = String::new();
    if chomp {
        out.push_str("chomp(");
    }
    out.push_str(if indented { "<<-" } else { "<<" });
    out.push_str(&delimiter);
    out.push('\n');
    for line in &lines {
        if indented && !line.is_empty() {
            indent(&mut out, level + 1);
        }
        out.push_str(&escape_template(line));
        out.push('\n');
    }
    if indented {
        indent(&mut out, level + 1);
    }
    out.push_str(&delimiter);
    if chomp {
        out.push(')');
    }
    out
}

/// `SQL`, or `SQL1`, `SQL2`, ... when a line would end the heredoc early.
fn delimiter(lines: &[&str]) -> String {
    let ends_heredoc = |line: &str, delimiter: &str| {
        line.trim_start_matches([' ', '\t'])
            .strip_prefix(delimiter)
            .is_some_and(|rest| !rest.starts_with(is_id_continue))
    };
    std::iter::once("SQL".to_owned())
        .chain((1..).map(|n| format!("SQL{n}")))
        .find(|d| !lines.iter().any(|line| ends_heredoc(line, d)))
        .unwrap_or_default()
}

fn escape_template(line: &str) -> String {
    line.replace("${", "$${").replace("%{", "%%{")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn delimiters_avoid_lines_that_end_the_heredoc() {
        assert_eq!(delimiter(&["SELECT 1"]), "SQL");
        assert_eq!(delimiter(&["SQLX", "SQL-y", "SQL_z"]), "SQL");
        assert_eq!(delimiter(&["  SQL"]), "SQL1");
        assert_eq!(delimiter(&["\tSQL;"]), "SQL1");
        assert_eq!(delimiter(&["SQL SECURITY DEFINER", "SQL1"]), "SQL2");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("é→"), "\"é→\"");
        assert_eq!(quote("$ % ${ %{ $${"), r#""$ % $${ %%{ $$${""#);
        assert_eq!(quote("\u{7f}\u{0}"), r#""\u007F\u0000""#);
    }

    #[test]
    fn numbers_stay_parseable() {
        assert_eq!(number_text("18446744073709551615"), "18446744073709551615");
        assert_eq!(
            number_text("18446744073709551616"),
            "18446744073709551616.0"
        );
        assert_eq!(
            number_text("-18446744073709551616"),
            "-18446744073709551616.0"
        );
        assert_eq!(number_text("1.5"), "1.5");
    }

    #[test]
    fn bare_words() {
        assert!(is_bare("CASCADE"));
        assert!(is_bare("s3-uploader"));
        assert!(!is_bare("double precision"));
        assert!(!is_bare("2fa"));
        assert!(!is_bare("null"));
        assert!(!is_bare("each"));
    }
}
