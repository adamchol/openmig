//! Parser for the canonical DDL ClickHouse reports in `system.tables.create_table_query` and
//! `system.functions.create_query`.
//!
//! The system tables do not expose everything the model needs (engine arguments, TTL, settings,
//! projections, constraints, materialized view targets, refresh clauses, dictionary sources), so
//! these clauses are recovered from the DDL. The input is always the server's own formatting:
//! keywords are upper case, column names in column lists are back-quoted, and clauses come in a
//! fixed order. Keywords are therefore matched case-sensitively, which keeps lower-case identifiers
//! such as a column named `ttl` from being mistaken for clauses. An unquoted identifier that is an
//! upper-case keyword (ClickHouse prints `TTL` unquoted) can still confuse the parser; such
//! schemas are not supported.
//!
//! The server keeps parentheses written around a whole expression (`DEFAULT (now())`,
//! `ORDER BY ((a), (b))`, `INDEX i ((x)) TYPE minmax`), so the same schema written with and
//! without them would print differently. Expressions in keys, partition and sample clauses, TTLs,
//! column defaults, index and constraint expressions and function bodies are therefore returned
//! normalized by [`normalize_expression`].

use openmig_core::Dialect;
use openmig_core::sql::{TokenKind, tokenize};
use std::collections::BTreeMap;
use std::ops::Range;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("cannot parse `{ddl}`: {message}")]
pub struct DdlError {
    pub message: String,
    pub ddl: String,
}

pub type Result<T> = std::result::Result<T, DdlError>;

/// `database.name` as written after `CREATE ... ` or `TO`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedName {
    pub database: Option<String>,
    pub name: String,
}

/// Engine and storage clauses of a table or of a materialized view's inner table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageClauses {
    /// `ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')`.
    pub engine: String,
    pub partition_by: Option<String>,
    /// Explicit `PRIMARY KEY` elements; `None` when the clause is absent.
    pub primary_key: Option<Vec<String>>,
    pub order_by: Option<Vec<String>>,
    pub sample_by: Option<String>,
    pub ttl: Option<String>,
    pub settings: BTreeMap<String, String>,
    /// `TimeSeries` target clauses, keyed by `DATA`, `TAGS` and `METRICS`.
    pub targets: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedColumn {
    pub name: String,
    pub data_type: String,
    /// `DEFAULT`, `MATERIALIZED`, `ALIAS` or `EPHEMERAL` and the expression (empty for a bare
    /// `EPHEMERAL`).
    pub default: Option<(String, String)>,
    pub comment: Option<String>,
    /// Codec list without `CODEC(...)`: `["Delta(8)", "ZSTD(1)"]`.
    pub codec: Vec<String>,
    /// Statistics types without `STATISTICS(...)`.
    pub statistics: Vec<String>,
    pub ttl: Option<String>,
    pub collate: Option<String>,
    pub settings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedIndex {
    pub name: String,
    pub expression: String,
    /// `bloom_filter(0.01)`.
    pub index_type: String,
    pub granularity: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedProjection {
    pub name: String,
    /// The query inside the parentheses.
    pub query: String,
    /// `WITH SETTINGS (...)`.
    pub settings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedConstraint {
    pub name: String,
    /// `CHECK` or `ASSUME`.
    pub kind: String,
    pub expression: String,
}

/// The parenthesised element list of `CREATE TABLE` (and the column list of views).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Elements {
    pub columns: Vec<ParsedColumn>,
    pub indexes: Vec<ParsedIndex>,
    pub projections: Vec<ParsedProjection>,
    pub constraints: Vec<ParsedConstraint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTable {
    pub name: ParsedName,
    pub elements: Elements,
    pub storage: StorageClauses,
    pub comment: Option<String>,
}

/// `CREATE VIEW` and `CREATE MATERIALIZED VIEW`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedView {
    pub name: Option<ParsedName>,
    pub materialized: bool,
    /// Text after `REFRESH`, e.g. `EVERY 1 HOUR APPEND`.
    pub refresh: Option<String>,
    pub to: Option<ParsedName>,
    pub columns: Vec<ParsedColumn>,
    pub storage: Option<StorageClauses>,
    pub definer: Option<String>,
    pub sql_security: Option<String>,
    pub comment: Option<String>,
    pub query: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedAttribute {
    pub name: String,
    pub data_type: String,
    pub default: Option<String>,
    pub expression: Option<String>,
    pub hierarchical: bool,
    pub bidirectional: bool,
    pub injective: bool,
    pub is_object_id: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedDictionary {
    pub name: Option<ParsedName>,
    pub attributes: Vec<ParsedAttribute>,
    pub primary_key: Vec<String>,
    /// Content of `SOURCE(...)`, e.g. `CLICKHOUSE(TABLE 'events' DB 'app')`.
    pub source: Option<String>,
    /// Content of `LIFETIME(...)`, e.g. `MIN 0 MAX 300`.
    pub lifetime: Option<String>,
    /// Content of `LAYOUT(...)`, e.g. `HASHED()`.
    pub layout: Option<String>,
    /// Content of `RANGE(...)`.
    pub range: Option<String>,
    pub settings: BTreeMap<String, String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFunction {
    pub name: String,
    pub parameters: Vec<String>,
    pub body: String,
}

/// The kind of object a `create_table_query` creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateKind {
    Table,
    View,
    MaterializedView,
    Dictionary,
    /// `LIVE VIEW`, `WINDOW VIEW` and anything else.
    Other,
}

pub fn create_kind(ddl: &str) -> CreateKind {
    let words: Vec<&str> = ddl.split_whitespace().take(4).collect();
    let words = match words.as_slice() {
        ["CREATE" | "ATTACH", "OR", "REPLACE", rest @ ..] => rest.to_vec(),
        ["CREATE" | "ATTACH", rest @ ..] => rest.to_vec(),
        _ => return CreateKind::Other,
    };
    match words.as_slice() {
        ["TABLE", ..] => CreateKind::Table,
        ["VIEW", ..] => CreateKind::View,
        ["MATERIALIZED", "VIEW", ..] => CreateKind::MaterializedView,
        ["DICTIONARY", ..] => CreateKind::Dictionary,
        _ => CreateKind::Other,
    }
}

const TABLE_CLAUSES: &[&[&str]] = &[
    &["PARTITION", "BY"],
    &["PRIMARY", "KEY"],
    &["ORDER", "BY"],
    &["SAMPLE", "BY"],
    &["TTL"],
    &["SETTINGS"],
    &["COMMENT"],
];

const TIME_SERIES_TARGETS: &[&str] = &["DATA", "TAGS", "METRICS"];

/// Keywords that can follow a column's type.
const COLUMN_CLAUSES: &[&str] = &[
    "NULL",
    "NOT",
    "DEFAULT",
    "MATERIALIZED",
    "ALIAS",
    "EPHEMERAL",
    "COMMENT",
    "CODEC",
    "STATISTICS",
    "TTL",
    "COLLATE",
    "SETTINGS",
    "PRIMARY",
];

/// Keywords that end a column's default or TTL expression. `NULL` and `NOT` are left out because
/// they can be part of an expression (`DEFAULT NULL`, `a NOT IN (1)`) and are printed before the
/// default.
const COLUMN_EXPRESSION_END: &[&str] = &[
    "COMMENT",
    "CODEC",
    "STATISTICS",
    "TTL",
    "COLLATE",
    "SETTINGS",
    "PRIMARY",
];

/// Parse `CREATE TABLE`.
pub fn parse_table(ddl: &str) -> Result<ParsedTable> {
    let p = Parser::new(ddl)?;
    let mut pos = p.expect_words(0, &["CREATE", "TABLE"])?;
    let (name, next) = p.qualified_name(pos)?;
    pos = p.skip_uuid(next);
    let mut elements = Elements::default();
    if p.is_punct(pos, "(") {
        let close = p.closing(pos)?;
        elements = p.elements(pos + 1..close)?;
        pos = close + 1;
    }
    if !p.is_word(pos, "ENGINE") {
        return Err(p.error(format!("expected ENGINE at {}", p.describe(pos))));
    }
    let (storage, comment, end) = p.storage(pos, &[])?;
    if end != p.len() {
        return Err(p.error(format!("unexpected {}", p.describe(end))));
    }
    Ok(ParsedTable {
        name,
        elements,
        storage,
        comment,
    })
}

/// Parse `CREATE VIEW` or `CREATE MATERIALIZED VIEW`.
pub fn parse_view(ddl: &str) -> Result<ParsedView> {
    let p = Parser::new(ddl)?;
    let mut view = ParsedView::default();
    let mut pos = p.expect_words(0, &["CREATE"])?;
    if p.is_word(pos, "MATERIALIZED") {
        view.materialized = true;
        pos += 1;
    }
    pos = p.expect_words(pos, &["VIEW"])?;
    let (name, next) = p.qualified_name(pos)?;
    view.name = Some(name);
    pos = p.skip_uuid(next);
    const STOP: &[&str] = &[
        "TO", "ENGINE", "DEFINER", "SQL", "COMMENT", "AS", "EMPTY", "POPULATE",
    ];
    while pos < p.len() {
        if p.is_word(pos, "REFRESH") {
            let end = p
                .find_top(pos + 1..p.len(), |i| {
                    p.is_punct(i, "(") || STOP.iter().any(|w| p.is_word(i, w))
                })
                .unwrap_or(p.len());
            view.refresh = Some(p.text(pos + 1..end).to_owned());
            pos = end;
        } else if p.is_word(pos, "TO") {
            let (to, next) = p.qualified_name(pos + 1)?;
            view.to = Some(to);
            pos = p.skip_uuid(next);
        } else if p.is_punct(pos, "(") {
            let close = p.closing(pos)?;
            view.columns = p.elements(pos + 1..close)?.columns;
            pos = close + 1;
        } else if p.is_word(pos, "ENGINE") {
            let (storage, _, end) =
                p.storage(pos, &["DEFINER", "SQL", "AS", "EMPTY", "POPULATE"])?;
            view.storage = Some(storage);
            pos = end;
        } else if p.is_word(pos, "DEFINER") {
            pos = p.expect_punct(pos + 1, "=")?;
            view.definer = Some(p.ident(pos)?);
            pos += 1;
        } else if p.is_word(pos, "SQL") && p.is_word(pos + 1, "SECURITY") {
            view.sql_security = Some(p.word(pos + 2)?.to_owned());
            pos += 3;
        } else if p.is_word(pos, "COMMENT") {
            view.comment = Some(p.string(pos + 1)?);
            pos += 2;
        } else if p.is_word(pos, "EMPTY") || p.is_word(pos, "POPULATE") {
            pos += 1;
        } else if p.is_word(pos, "AS") {
            view.query = p.text(pos + 1..p.len()).to_owned();
            pos = p.len();
        } else {
            return Err(p.error(format!("unexpected {}", p.describe(pos))));
        }
    }
    Ok(view)
}

/// Parse `CREATE DICTIONARY`.
pub fn parse_dictionary(ddl: &str) -> Result<ParsedDictionary> {
    let p = Parser::new(ddl)?;
    let mut dict = ParsedDictionary::default();
    let pos = p.expect_words(0, &["CREATE", "DICTIONARY"])?;
    let (name, next) = p.qualified_name(pos)?;
    dict.name = Some(name);
    let mut pos = p.skip_uuid(next);
    if p.is_punct(pos, "(") {
        let close = p.closing(pos)?;
        for range in p.split_commas(pos + 1..close) {
            dict.attributes.push(p.attribute(range)?);
        }
        pos = close + 1;
    }
    while pos < p.len() {
        let paren = |what: &str| -> Result<(String, usize)> {
            if !p.is_punct(pos + 1, "(") {
                return Err(p.error(format!("expected ( after {what}")));
            }
            let close = p.closing(pos + 1)?;
            Ok((p.text(pos + 2..close).to_owned(), close + 1))
        };
        if p.is_word(pos, "PRIMARY") && p.is_word(pos + 1, "KEY") {
            let end = p
                .find_top(pos + 2..p.len(), |i| {
                    [
                        "SOURCE", "LIFETIME", "LAYOUT", "RANGE", "SETTINGS", "COMMENT",
                    ]
                    .iter()
                    .any(|w| p.is_word(i, w) && p.is_punct_or_string_after(i))
                })
                .unwrap_or(p.len());
            dict.primary_key = p
                .split_commas(pos + 2..end)
                .into_iter()
                .map(|r| p.text(r).to_owned())
                .collect();
            pos = end;
        } else if p.is_word(pos, "SOURCE") {
            let (text, next) = paren("SOURCE")?;
            dict.source = Some(text);
            pos = next;
        } else if p.is_word(pos, "LIFETIME") {
            let (text, next) = paren("LIFETIME")?;
            dict.lifetime = Some(text);
            pos = next;
        } else if p.is_word(pos, "LAYOUT") {
            let (text, next) = paren("LAYOUT")?;
            dict.layout = Some(text);
            pos = next;
        } else if p.is_word(pos, "RANGE") {
            let (text, next) = paren("RANGE")?;
            dict.range = Some(text);
            pos = next;
        } else if p.is_word(pos, "SETTINGS") {
            if !p.is_punct(pos + 1, "(") {
                return Err(p.error("expected ( after SETTINGS"));
            }
            let close = p.closing(pos + 1)?;
            dict.settings = p.settings(pos + 2..close)?;
            pos = close + 1;
        } else if p.is_word(pos, "COMMENT") {
            dict.comment = Some(p.string(pos + 1)?);
            pos += 2;
        } else {
            return Err(p.error(format!("unexpected {}", p.describe(pos))));
        }
    }
    Ok(dict)
}

/// Parse `CREATE FUNCTION name AS (x, y) -> body`.
pub fn parse_function(ddl: &str) -> Result<ParsedFunction> {
    let p = Parser::new(ddl)?;
    let pos = p.expect_words(0, &["CREATE", "FUNCTION"])?;
    let name = p.ident(pos)?;
    let mut pos = pos + 1;
    if p.is_word(pos, "ON") && p.is_word(pos + 1, "CLUSTER") {
        pos += 3;
    }
    pos = p.expect_words(pos, &["AS"])?;
    let arrow = p
        .find_top(pos..p.len(), |i| {
            p.is_punct(i, "-") && p.is_punct(i + 1, ">") && p.toks[i].end == p.toks[i + 1].start
        })
        .ok_or_else(|| p.error("expected -> in the function definition"))?;
    let parameters = if p.is_punct(pos, "(") {
        let close = p.closing(pos)?;
        if close + 1 != arrow {
            return Err(p.error("unexpected tokens before ->"));
        }
        p.split_commas(pos + 1..close)
            .into_iter()
            .map(|r| p.ident(r.start))
            .collect::<Result<Vec<_>>>()?
    } else if arrow == pos + 1 {
        vec![p.ident(pos)?]
    } else {
        return Err(p.error("unexpected tokens before ->"));
    };
    if arrow + 2 >= p.len() {
        return Err(p.error("empty function body"));
    }
    Ok(ParsedFunction {
        name,
        parameters,
        body: p.normalize(arrow + 2..p.len()),
    })
}

/// An expression without redundant parentheses around it: `(a + 1)` becomes `a + 1` and
/// `((a), (b))` becomes `(a, b)`. Parentheses inside the expression are kept; the server prints
/// those canonically anyway. Scalar subqueries keep theirs.
pub fn normalize_expression(text: &str) -> String {
    match Parser::new(text) {
        Ok(p) => p.normalize(0..p.len()),
        Err(_) => text.trim().to_owned(),
    }
}

/// A TTL clause with [`normalize_expression`] applied to each element's expression and `WHERE`
/// condition. Clauses with `GROUP BY` (whose `SET` list contains top-level commas) are kept as
/// they are.
pub fn normalize_ttl(text: &str) -> String {
    match Parser::new(text) {
        Ok(p) => p.ttl(0..p.len()),
        Err(_) => text.trim().to_owned(),
    }
}

/// Split a key expression (`(a, b)`, `tuple(a, b DESC)`, `a`, `tuple()`) into its elements.
pub fn split_key(text: &str) -> Result<Vec<String>> {
    let p = Parser::new(text)?;
    Ok(p.key(0..p.len()))
}

/// Split a comma-separated list at the top level (`Delta(8), ZSTD(1)`).
pub fn split_list(text: &str) -> Result<Vec<String>> {
    let p = Parser::new(text)?;
    Ok(p.split_commas(0..p.len())
        .into_iter()
        .map(|r| p.text(r).to_owned())
        .collect())
}

/// The value of a ClickHouse string literal (`'it\'s'`).
pub fn unquote_string(literal: &str) -> Option<String> {
    let inner = literal.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(unescape(inner, '\''))
}

/// The name in a quoted identifier (`` `a\`b` `` or `"a"`), or the text of a bare one.
pub fn unquote_ident(text: &str) -> String {
    for quote in ['`', '"'] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|t| t.strip_suffix(quote)) {
            return unescape(inner, quote);
        }
    }
    text.to_owned()
}

fn unescape(inner: &str, quote: char) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == quote && chars.peek() == Some(&quote) {
            chars.next();
            out.push(quote);
        } else if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('a') => out.push('\u{7}'),
                Some('v') => out.push('\u{b}'),
                Some('e') => out.push('\u{1b}'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct Tok<'a> {
    kind: TokenKind,
    text: &'a str,
    start: usize,
    end: usize,
    /// Bracket nesting outside this token: an opening bracket and its closing bracket have the
    /// same depth, tokens between them one more.
    depth: usize,
}

/// Significant tokens of one statement and helpers over token index ranges.
struct Parser<'a> {
    src: &'a str,
    toks: Vec<Tok<'a>>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Result<Self> {
        let tokens = tokenize(src, Dialect::ClickHouse).map_err(|e| DdlError {
            message: e.to_string(),
            ddl: src.to_owned(),
        })?;
        let mut toks = Vec::with_capacity(tokens.len());
        let mut depth = 0usize;
        for token in tokens {
            if matches!(
                token.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            ) {
                continue;
            }
            let punct = token.kind == TokenKind::Punct;
            if punct && matches!(token.text, ")" | "]" | "}") {
                depth = depth.saturating_sub(1);
            }
            toks.push(Tok {
                kind: token.kind,
                text: token.text,
                start: token.offset,
                end: token.offset + token.text.len(),
                depth,
            });
            if punct && matches!(token.text, "(" | "[" | "{") {
                depth += 1;
            }
        }
        if toks.last().is_some_and(|t| t.text == ";") {
            toks.pop();
        }
        Ok(Self { src, toks })
    }

    fn len(&self) -> usize {
        self.toks.len()
    }

    fn error(&self, message: impl Into<String>) -> DdlError {
        DdlError {
            message: message.into(),
            ddl: self.src.to_owned(),
        }
    }

    fn describe(&self, pos: usize) -> String {
        match self.toks.get(pos) {
            Some(t) => format!("`{}`", t.text),
            None => "end of statement".to_owned(),
        }
    }

    /// Source text covering the tokens in `range` (empty for an empty range).
    fn text(&self, range: Range<usize>) -> &'a str {
        if range.start >= range.end || range.start >= self.toks.len() {
            return "";
        }
        let end = range.end.min(self.toks.len());
        &self.src[self.toks[range.start].start..self.toks[end - 1].end]
    }

    fn is_word(&self, pos: usize, word: &str) -> bool {
        self.toks
            .get(pos)
            .is_some_and(|t| t.kind == TokenKind::Word && t.text == word)
    }

    fn is_punct(&self, pos: usize, punct: &str) -> bool {
        self.toks
            .get(pos)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text == punct)
    }

    /// Whether the token after `pos` opens a parenthesis or is a string (a dictionary clause
    /// keyword is always followed by one of them).
    fn is_punct_or_string_after(&self, pos: usize) -> bool {
        self.is_punct(pos + 1, "(")
            || self
                .toks
                .get(pos + 1)
                .is_some_and(|t| t.kind == TokenKind::String)
    }

    fn word(&self, pos: usize) -> Result<&'a str> {
        match self.toks.get(pos) {
            Some(t) if t.kind == TokenKind::Word => Ok(t.text),
            _ => Err(self.error(format!("expected a keyword at {}", self.describe(pos)))),
        }
    }

    fn ident(&self, pos: usize) -> Result<String> {
        match self.toks.get(pos) {
            Some(t) if matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent) => {
                Ok(unquote_ident(t.text))
            }
            _ => Err(self.error(format!("expected a name at {}", self.describe(pos)))),
        }
    }

    fn string(&self, pos: usize) -> Result<String> {
        match self.toks.get(pos) {
            Some(t) if t.kind == TokenKind::String => unquote_string(t.text)
                .ok_or_else(|| self.error(format!("unsupported string literal {}", t.text))),
            _ => Err(self.error(format!("expected a string at {}", self.describe(pos)))),
        }
    }

    fn expect_words(&self, mut pos: usize, words: &[&str]) -> Result<usize> {
        for word in words {
            if !self.is_word(pos, word) {
                return Err(self.error(format!("expected {word} at {}", self.describe(pos))));
            }
            pos += 1;
        }
        Ok(pos)
    }

    fn expect_punct(&self, pos: usize, punct: &str) -> Result<usize> {
        if self.is_punct(pos, punct) {
            Ok(pos + 1)
        } else {
            Err(self.error(format!("expected {punct} at {}", self.describe(pos))))
        }
    }

    /// Index of the bracket closing the one at `open`.
    fn closing(&self, open: usize) -> Result<usize> {
        let depth = self.toks[open].depth;
        let close = match self.toks[open].text {
            "(" => ")",
            "[" => "]",
            _ => "}",
        };
        (open + 1..self.toks.len())
            .find(|&i| self.toks[i].depth == depth && self.is_punct(i, close))
            .ok_or_else(|| self.error("unbalanced brackets"))
    }

    /// First index in `range` at the depth of `range.start` for which `pred` holds.
    fn find_top(&self, range: Range<usize>, pred: impl Fn(usize) -> bool) -> Option<usize> {
        let depth = self.toks.get(range.start)?.depth;
        range
            .into_iter()
            .find(|&i| self.toks[i].depth == depth && pred(i))
    }

    fn split_commas(&self, range: Range<usize>) -> Vec<Range<usize>> {
        let Some(first) = self.toks.get(range.start) else {
            return Vec::new();
        };
        if range.start >= range.end {
            return Vec::new();
        }
        let depth = first.depth;
        let mut out = Vec::new();
        let mut start = range.start;
        for i in range.clone() {
            if self.toks[i].depth == depth && self.is_punct(i, ",") {
                out.push(start..i);
                start = i + 1;
            }
        }
        out.push(start..range.end);
        out.retain(|r| r.start < r.end);
        out
    }

    fn qualified_name(&self, pos: usize) -> Result<(ParsedName, usize)> {
        let first = self.ident(pos)?;
        if self.is_punct(pos + 1, ".") {
            let name = self.ident(pos + 2)?;
            Ok((
                ParsedName {
                    database: Some(first),
                    name,
                },
                pos + 3,
            ))
        } else {
            Ok((
                ParsedName {
                    database: None,
                    name: first,
                },
                pos + 1,
            ))
        }
    }

    fn skip_uuid(&self, pos: usize) -> usize {
        if self.is_word(pos, "UUID")
            && self
                .toks
                .get(pos + 1)
                .is_some_and(|t| t.kind == TokenKind::String)
        {
            pos + 2
        } else {
            pos
        }
    }

    /// Elements of a key expression: a parenthesised or `tuple(...)` list is split, anything else
    /// is one element.
    fn key(&self, range: Range<usize>) -> Vec<String> {
        if range.start >= range.end {
            return Vec::new();
        }
        let open = if self.is_punct(range.start, "(") {
            Some(range.start)
        } else if self.is_word(range.start, "tuple") && self.is_punct(range.start + 1, "(") {
            Some(range.start + 1)
        } else {
            None
        };
        if let Some(open) = open
            && self.closing(open).ok() == Some(range.end - 1)
        {
            return self
                .split_commas(open + 1..range.end - 1)
                .into_iter()
                .map(|r| self.normalize(r))
                .collect();
        }
        vec![self.normalize(range)]
    }

    /// See [`normalize_expression`].
    fn normalize(&self, range: Range<usize>) -> String {
        let mut range = range;
        while range.end >= range.start + 3
            && self.is_punct(range.start, "(")
            && self.closing(range.start).ok() == Some(range.end - 1)
        {
            let inner = range.start + 1..range.end - 1;
            let parts = self.split_commas(inner.clone());
            if parts.len() > 1 {
                let parts: Vec<String> = parts.into_iter().map(|r| self.normalize(r)).collect();
                return format!("({})", parts.join(", "));
            }
            if self.is_word(inner.start, "SELECT") || self.is_word(inner.start, "WITH") {
                break;
            }
            range = inner;
        }
        self.text(range).to_owned()
    }

    /// See [`normalize_ttl`].
    fn ttl(&self, range: Range<usize>) -> String {
        let depth = self.toks.get(range.start).map_or(0, |t| t.depth);
        if (range.clone()).any(|i| self.toks[i].depth == depth && self.is_word(i, "GROUP")) {
            return self.text(range).to_owned();
        }
        let elements: Vec<String> = self
            .split_commas(range)
            .into_iter()
            .map(|element| {
                let action = |i: usize| {
                    self.toks[i].depth == depth
                        && ["DELETE", "TO", "RECOMPRESS", "WHERE"]
                            .iter()
                            .any(|w| self.is_word(i, w))
                };
                let expression_end = (element.start..element.end)
                    .find(|&i| action(i))
                    .unwrap_or(element.end);
                let mut out = self.normalize(element.start..expression_end);
                let where_pos = (expression_end..element.end)
                    .find(|&i| self.toks[i].depth == depth && self.is_word(i, "WHERE"));
                let actions_end = where_pos.unwrap_or(element.end);
                if expression_end < actions_end {
                    out.push(' ');
                    out.push_str(self.text(expression_end..actions_end));
                }
                if let Some(w) = where_pos {
                    out.push_str(" WHERE ");
                    out.push_str(&self.normalize(w + 1..element.end));
                }
                out
            })
            .collect();
        elements.join(", ")
    }

    /// `k = v, k2 = v2` into a map of key to value text.
    fn settings(&self, range: Range<usize>) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for item in self.split_commas(range) {
            let key = self.ident(item.start)?;
            let value = self.expect_punct(item.start + 1, "=")?;
            if value >= item.end {
                return Err(self.error(format!("setting `{key}` has no value")));
            }
            out.insert(key, self.text(value..item.end).to_owned());
        }
        Ok(out)
    }

    /// `ENGINE = ...` and the storage clauses after it, up to the end of the statement or one of
    /// `stop` (top-level words). Returns the clauses, the table comment and the index where
    /// parsing stopped.
    fn storage(
        &self,
        pos: usize,
        stop: &[&str],
    ) -> Result<(StorageClauses, Option<String>, usize)> {
        let mut storage = StorageClauses::default();
        let engine_start = self.expect_punct(pos + 1, "=")?;
        self.word(engine_start)?;
        let mut engine_end = engine_start + 1;
        if self.is_punct(engine_end, "(") {
            engine_end = self.closing(engine_end)? + 1;
        }
        storage.engine = self.text(engine_start..engine_end).to_owned();
        let time_series = self.toks[engine_start].text == "TimeSeries";
        let depth = self.toks[pos].depth;

        let end = (engine_end..self.len())
            .find(|&i| self.toks[i].depth == depth && stop.iter().any(|w| self.is_word(i, w)))
            .unwrap_or(self.len());

        let mut starts: Vec<(usize, usize, &str)> = Vec::new();
        let mut i = engine_end;
        while i < end {
            if self.toks[i].depth == depth {
                if time_series && TIME_SERIES_TARGETS.iter().any(|w| self.is_word(i, w)) {
                    starts.push((i, i + 1, self.toks[i].text));
                    i += 1;
                    continue;
                }
                let in_target = starts
                    .last()
                    .is_some_and(|(_, _, k)| TIME_SERIES_TARGETS.contains(k));
                if !in_target
                    && let Some(clause) = TABLE_CLAUSES.iter().find(|clause| {
                        clause
                            .iter()
                            .enumerate()
                            .all(|(n, word)| self.is_word(i + n, word))
                    })
                {
                    starts.push((i, i + clause.len(), clause[0]));
                    i += clause.len();
                    continue;
                }
            }
            i += 1;
        }
        if let Some(&(first, _, _)) = starts.first()
            && first != engine_end
        {
            return Err(self.error(format!(
                "unexpected {} after the engine",
                self.describe(engine_end)
            )));
        }
        if starts.is_empty() && engine_end != end {
            return Err(self.error(format!(
                "unexpected {} after the engine",
                self.describe(engine_end)
            )));
        }

        let mut comment = None;
        for (n, &(_, body_start, keyword)) in starts.iter().enumerate() {
            let body_end = starts.get(n + 1).map_or(end, |&(next, _, _)| next);
            let body = body_start..body_end;
            if body.start >= body.end {
                return Err(self.error(format!("empty {keyword} clause")));
            }
            let text = self.text(body.clone()).to_owned();
            match keyword {
                "PARTITION" => storage.partition_by = Some(self.normalize(body)),
                "PRIMARY" => storage.primary_key = Some(self.key(body)),
                "ORDER" => storage.order_by = Some(self.key(body)),
                "SAMPLE" => storage.sample_by = Some(self.normalize(body)),
                "TTL" => storage.ttl = Some(self.ttl(body)),
                "SETTINGS" => storage.settings = self.settings(body)?,
                "COMMENT" => comment = Some(self.string(body.start)?),
                target => {
                    storage.targets.insert(target.to_owned(), text);
                }
            }
        }
        Ok((storage, comment, end))
    }

    fn elements(&self, range: Range<usize>) -> Result<Elements> {
        let mut elements = Elements::default();
        for item in self.split_commas(range) {
            let first = &self.toks[item.start];
            let keyword = first.kind == TokenKind::Word;
            if keyword && first.text == "INDEX" {
                elements.indexes.push(self.index(item)?);
            } else if keyword && first.text == "PROJECTION" {
                elements.projections.push(self.projection(item)?);
            } else if keyword && first.text == "CONSTRAINT" {
                elements.constraints.push(self.constraint(item)?);
            } else if keyword && first.text == "PRIMARY" && self.is_word(item.start + 1, "KEY") {
                continue;
            } else {
                elements.columns.push(self.column(item)?);
            }
        }
        Ok(elements)
    }

    /// `INDEX name expr TYPE type [GRANULARITY n]`. The expression may contain the word `type`
    /// (a column), so the type is found from the end.
    fn index(&self, item: Range<usize>) -> Result<ParsedIndex> {
        let name = self.ident(item.start + 1)?;
        let depth = self.toks[item.start].depth;
        let mut end = item.end;
        let mut granularity = None;
        if end >= 2 && self.is_word(end - 2, "GRANULARITY") {
            granularity = Some(
                self.toks[end - 1]
                    .text
                    .parse()
                    .map_err(|_| self.error("invalid GRANULARITY"))?,
            );
            end -= 2;
        }
        let type_pos = (item.start + 2..end)
            .rev()
            .find(|&i| self.toks[i].depth == depth && self.is_word(i, "TYPE"))
            .ok_or_else(|| self.error(format!("index `{name}` has no TYPE")))?;
        Ok(ParsedIndex {
            name,
            expression: self.normalize(item.start + 2..type_pos),
            index_type: self.text(type_pos + 1..end).to_owned(),
            granularity,
        })
    }

    fn projection(&self, item: Range<usize>) -> Result<ParsedProjection> {
        let name = self.ident(item.start + 1)?;
        let open = self.expect_punct(item.start + 2, "(")? - 1;
        let close = self.closing(open)?;
        let mut settings = BTreeMap::new();
        let mut pos = close + 1;
        if pos < item.end {
            pos = self.expect_words(pos, &["WITH", "SETTINGS"])?;
            let open = self.expect_punct(pos, "(")? - 1;
            let close = self.closing(open)?;
            settings = self.settings(open + 1..close)?;
            pos = close + 1;
        }
        if pos != item.end {
            return Err(self.error(format!("unexpected {} in projection", self.describe(pos))));
        }
        Ok(ParsedProjection {
            name,
            query: self.text(open + 1..close).to_owned(),
            settings,
        })
    }

    fn constraint(&self, item: Range<usize>) -> Result<ParsedConstraint> {
        let name = self.ident(item.start + 1)?;
        let kind = self.word(item.start + 2)?;
        if kind != "CHECK" && kind != "ASSUME" {
            return Err(self.error(format!("unsupported constraint kind {kind}")));
        }
        Ok(ParsedConstraint {
            name,
            kind: kind.to_owned(),
            expression: self.normalize(item.start + 3..item.end),
        })
    }

    /// One column declaration: name, type, then clauses in any order.
    fn column(&self, item: Range<usize>) -> Result<ParsedColumn> {
        let mut column = ParsedColumn {
            name: self.ident(item.start)?,
            ..ParsedColumn::default()
        };
        let depth = self.toks[item.start].depth;
        let is_clause = |i: usize| {
            self.toks[i].depth == depth && COLUMN_CLAUSES.iter().any(|w| self.is_word(i, w))
        };
        let mut pos = item.start + 1;
        if pos < item.end && !is_clause(pos) {
            self.word(pos)?;
            let mut type_end = pos + 1;
            if self.is_punct(type_end, "(") {
                type_end = self.closing(type_end)? + 1;
            }
            column.data_type = self.text(pos..type_end).to_owned();
            pos = type_end;
        }
        let clause_end = |from: usize| {
            (from..item.end)
                .find(|&i| {
                    self.toks[i].depth == depth
                        && COLUMN_EXPRESSION_END.iter().any(|w| self.is_word(i, w))
                })
                .unwrap_or(item.end)
        };
        let paren_body = |pos: usize| -> Result<(Range<usize>, usize)> {
            let open = self.expect_punct(pos, "(")? - 1;
            let close = self.closing(open)?;
            Ok((open + 1..close, close + 1))
        };
        while pos < item.end {
            let word = self.word(pos)?;
            match word {
                "NULL" => pos += 1,
                "NOT" if self.is_word(pos + 1, "NULL") => pos += 2,
                "DEFAULT" | "MATERIALIZED" | "ALIAS" | "EPHEMERAL" => {
                    let end = clause_end(pos + 1);
                    column.default = Some((word.to_owned(), self.normalize(pos + 1..end)));
                    pos = end;
                }
                "COMMENT" => {
                    column.comment = Some(self.string(pos + 1)?);
                    pos += 2;
                }
                "CODEC" => {
                    let (body, next) = paren_body(pos + 1)?;
                    column.codec = self
                        .split_commas(body)
                        .into_iter()
                        .map(|r| self.text(r).to_owned())
                        .collect();
                    pos = next;
                }
                "STATISTICS" => {
                    let (body, next) = paren_body(pos + 1)?;
                    column.statistics = self
                        .split_commas(body)
                        .into_iter()
                        .map(|r| self.text(r).to_owned())
                        .collect();
                    pos = next;
                }
                "TTL" => {
                    let end = clause_end(pos + 1);
                    column.ttl = Some(self.normalize(pos + 1..end));
                    pos = end;
                }
                "COLLATE" => {
                    column.collate = Some(self.ident(pos + 1)?);
                    pos += 2;
                }
                "SETTINGS" => {
                    let (body, next) = paren_body(pos + 1)?;
                    column.settings = self.settings(body)?;
                    pos = next;
                }
                "PRIMARY" if self.is_word(pos + 1, "KEY") => pos += 2,
                _ => {
                    return Err(self.error(format!(
                        "unexpected {} in column `{}`",
                        self.describe(pos),
                        column.name
                    )));
                }
            }
        }
        Ok(column)
    }

    fn attribute(&self, item: Range<usize>) -> Result<ParsedAttribute> {
        let mut attr = ParsedAttribute {
            name: self.ident(item.start)?,
            ..ParsedAttribute::default()
        };
        let depth = self.toks[item.start].depth;
        const CLAUSES: &[&str] = &[
            "DEFAULT",
            "EXPRESSION",
            "HIERARCHICAL",
            "BIDIRECTIONAL",
            "INJECTIVE",
            "IS_OBJECT_ID",
        ];
        let is_clause =
            |i: usize| self.toks[i].depth == depth && CLAUSES.iter().any(|w| self.is_word(i, w));
        let mut pos = item.start + 1;
        self.word(pos)?;
        let mut type_end = pos + 1;
        if self.is_punct(type_end, "(") {
            type_end = self.closing(type_end)? + 1;
        }
        attr.data_type = self.text(pos..type_end).to_owned();
        pos = type_end;
        let clause_end = |from: usize| (from..item.end).find(|&i| is_clause(i)).unwrap_or(item.end);
        while pos < item.end {
            match self.word(pos)? {
                "DEFAULT" => {
                    let end = clause_end(pos + 1);
                    attr.default = Some(self.text(pos + 1..end).to_owned());
                    pos = end;
                }
                "EXPRESSION" => {
                    let end = clause_end(pos + 1);
                    attr.expression = Some(self.text(pos + 1..end).to_owned());
                    pos = end;
                }
                "HIERARCHICAL" => {
                    attr.hierarchical = true;
                    pos += 1;
                }
                "BIDIRECTIONAL" => {
                    attr.bidirectional = true;
                    pos += 1;
                }
                "INJECTIVE" => {
                    attr.injective = true;
                    pos += 1;
                }
                "IS_OBJECT_ID" => {
                    attr.is_object_id = true;
                    pos += 1;
                }
                _ => {
                    return Err(self.error(format!(
                        "unexpected {} in attribute `{}`",
                        self.describe(pos),
                        attr.name
                    )));
                }
            }
        }
        Ok(attr)
    }
}
