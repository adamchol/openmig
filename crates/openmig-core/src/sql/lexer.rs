use super::{Dialect, SplitError, SplitStatement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    QuotedIdent,
    String,
    Number,
    Punct,
    LineComment,
    BlockComment,
    Whitespace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    pub offset: usize,
}

pub fn tokenize(_sql: &str, _dialect: Dialect) -> Result<Vec<Token<'_>>, SplitError> {
    unimplemented!("sql lexer")
}

pub(super) fn split(_sql: &str, _dialect: Dialect) -> Result<Vec<SplitStatement>, SplitError> {
    unimplemented!("sql splitter")
}

pub(super) fn identifiers(_sql: &str, _dialect: Dialect) -> Vec<String> {
    unimplemented!("sql identifiers")
}
