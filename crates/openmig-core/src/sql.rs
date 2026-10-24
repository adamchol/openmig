//! SQL lexing shared by both engines: splitting migration files into statements, scanning
//! identifiers for soft dependency hints, and quoting.

mod lexer;

pub use lexer::{Token, TokenKind, tokenize};

/// Which SQL dialect to lex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// Dollar quoting (`$$`, `$tag$`), `E'...'` strings with backslash escapes, standard `''`
    /// strings, `"identifiers"`, nested `/* */` comments, `BEGIN ATOMIC ... END` bodies.
    Postgres,
    /// Backslash escapes in `'...'`, `` `identifiers` `` and `"identifiers"`, non-nested `/* */`
    /// comments, `#` and `--` line comments, `$heredoc$` strings.
    ClickHouse,
}

/// One statement of a SQL script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitStatement {
    /// The statement text, trimmed, without the terminating `;` and without the comment lines
    /// directly above it.
    pub sql: String,
    /// `--` comment lines directly above the statement (text after `--`, trimmed).
    pub comments: Vec<String>,
    /// 1-based line where the statement text starts.
    pub line: usize,
    /// Byte offset where the statement text starts.
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct SplitError {
    pub line: usize,
    pub message: String,
}

/// Split a script into statements on top-level `;`. Empty statements are dropped; a trailing
/// statement without `;` is kept.
pub fn split(sql: &str, dialect: Dialect) -> Result<Vec<SplitStatement>, SplitError> {
    lexer::split(sql, dialect)
}

/// Every identifier-like word and quoted identifier in `sql`, outside strings and comments,
/// unquoted words lower-cased (PostgreSQL) or kept (ClickHouse). Used to guess dependencies of
/// function bodies and view queries.
pub fn identifiers(sql: &str, dialect: Dialect) -> Vec<String> {
    lexer::identifiers(sql, dialect)
}

/// Quote an identifier: `"name"` for PostgreSQL (embedded `"` doubled), `` `name` `` for
/// ClickHouse (embedded `` ` `` and `\` escaped).
pub fn quote_ident(name: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
        Dialect::ClickHouse => format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`")),
    }
}

/// Quote a string literal: `'it''s'` for PostgreSQL (`E''` is never needed because
/// `standard_conforming_strings` is on), `'it\'s'` for ClickHouse.
pub fn quote_literal(value: &str, dialect: Dialect) -> String {
    match dialect {
        Dialect::Postgres => format!("'{}'", value.replace('\'', "''")),
        Dialect::ClickHouse => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
    }
}

/// A dollar-quote tag that does not occur in `body`: `$$`, then `$_$`, `$__$`, ...
pub fn dollar_tag(body: &str) -> String {
    let mut tag = String::from("$$");
    let mut n = 0;
    while body.contains(&tag) {
        n += 1;
        tag = format!("${}$", "_".repeat(n));
    }
    tag
}
