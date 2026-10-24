use super::{Dialect, SplitError, SplitStatement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    QuotedIdent,
    /// String literals of every form, including PostgreSQL dollar-quoted bodies and ClickHouse
    /// heredocs.
    String,
    Number,
    /// A single punctuation character, or a PostgreSQL positional parameter (`$1`).
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

/// Lex `sql` into tokens. The tokens cover the input without gaps: concatenating their texts
/// gives back `sql`. Line comments end before the line break (and before a `\r` preceding it).
pub fn tokenize(sql: &str, dialect: Dialect) -> Result<Vec<Token<'_>>, SplitError> {
    Lexer::new(sql, dialect).collect()
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    dialect: Dialect,
    failed: bool,
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Result<Token<'a>, SplitError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.bytes.len() {
            return None;
        }
        let start = self.pos;
        match self.scan() {
            Ok(kind) => Some(Ok(Token {
                kind,
                text: &self.src[start..self.pos],
                offset: start,
            })),
            Err(message) => {
                self.failed = true;
                Some(Err(SplitError {
                    line: line_at(self.src, start),
                    message,
                }))
            }
        }
    }
}

// Every delimiter the lexer looks for is ASCII and every non-ASCII byte is treated as an
// identifier character, so token boundaries always fall on UTF-8 character boundaries.
impl<'a> Lexer<'a> {
    fn new(src: &'a str, dialect: Dialect) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            dialect,
            failed: false,
        }
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn postgres(&self) -> bool {
        self.dialect == Dialect::Postgres
    }

    fn scan(&mut self) -> Result<TokenKind, String> {
        let c = self.bytes[self.pos];
        let next = self.peek(1);
        match c {
            _ if is_space(c) => {
                self.eat_while(is_space);
                Ok(TokenKind::Whitespace)
            }
            b'-' if next == Some(b'-') => {
                self.line_comment();
                Ok(TokenKind::LineComment)
            }
            // ClickHouse only treats `#` as a comment when followed by a space or `!` (shebang).
            b'#' if !self.postgres()
                && matches!(next, None | Some(b' ' | b'\t' | b'!' | b'\r' | b'\n')) =>
            {
                self.line_comment();
                Ok(TokenKind::LineComment)
            }
            b'/' if next == Some(b'*') => self.block_comment(),
            b'\'' => {
                self.quoted(b'\'', !self.postgres())?;
                Ok(TokenKind::String)
            }
            b'"' => {
                self.quoted(b'"', !self.postgres())?;
                Ok(TokenKind::QuotedIdent)
            }
            b'`' if !self.postgres() => {
                self.quoted(b'`', true)?;
                Ok(TokenKind::QuotedIdent)
            }
            b'$' => self.dollar(),
            b'0'..=b'9' => {
                self.number();
                Ok(TokenKind::Number)
            }
            b'.' if next.is_some_and(|n| n.is_ascii_digit()) => {
                self.number();
                Ok(TokenKind::Number)
            }
            _ if is_ident_start(c) => self.word(),
            _ => {
                self.pos += 1;
                Ok(TokenKind::Punct)
            }
        }
    }

    fn eat_while(&mut self, pred: impl Fn(u8) -> bool) {
        while self.pos < self.bytes.len() && pred(self.bytes[self.pos]) {
            self.pos += 1;
        }
    }

    fn line_comment(&mut self) {
        let start = self.pos;
        self.eat_while(|b| b != b'\n');
        if self.pos > start + 2 && self.bytes[self.pos - 1] == b'\r' {
            self.pos -= 1;
        }
    }

    fn block_comment(&mut self) -> Result<TokenKind, String> {
        self.pos += 2;
        let nested = self.postgres();
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            match (self.bytes[self.pos], self.peek(1)) {
                (b'*', Some(b'/')) => {
                    self.pos += 2;
                    depth -= 1;
                    if depth == 0 {
                        return Ok(TokenKind::BlockComment);
                    }
                }
                (b'/', Some(b'*')) if nested => {
                    self.pos += 2;
                    depth += 1;
                }
                _ => self.pos += 1,
            }
        }
        Err("unterminated block comment".to_owned())
    }

    /// Consume a quoted token starting at the opening `quote`. A doubled quote always escapes it;
    /// `backslash` additionally enables backslash escapes.
    fn quoted(&mut self, quote: u8, backslash: bool) -> Result<(), String> {
        self.pos += 1;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if backslash && c == b'\\' {
                self.pos += 2;
            } else if c == quote {
                if self.peek(1) == Some(quote) {
                    self.pos += 2;
                } else {
                    self.pos += 1;
                    return Ok(());
                }
            } else {
                self.pos += 1;
            }
        }
        self.pos = self.bytes.len();
        Err(match quote {
            b'\'' => "unterminated string literal".to_owned(),
            _ => "unterminated quoted identifier".to_owned(),
        })
    }

    fn dollar(&mut self) -> Result<TokenKind, String> {
        let postgres = self.postgres();
        if postgres && self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            self.eat_while(|b| b.is_ascii_digit());
            return Ok(TokenKind::Punct);
        }
        let tag_start = self.pos + 1;
        let mut tag_end = tag_start;
        while tag_end < self.bytes.len() && is_ident_char(self.bytes[tag_end]) {
            tag_end += 1;
        }
        if self.bytes.get(tag_end) != Some(&b'$') {
            self.pos += 1;
            return Ok(TokenKind::Punct);
        }
        let delimiter = &self.src[self.pos..=tag_end];
        let body = tag_end + 1;
        match self.src[body..].find(delimiter) {
            Some(close) => {
                self.pos = body + close + delimiter.len();
                Ok(TokenKind::String)
            }
            None => {
                self.pos = self.bytes.len();
                Err(if postgres {
                    format!("unterminated dollar-quoted string (missing closing {delimiter})")
                } else {
                    format!("unterminated heredoc (missing closing {delimiter})")
                })
            }
        }
    }

    fn number(&mut self) {
        let digits = |b: u8| b.is_ascii_digit() || b == b'_';
        self.eat_while(digits);
        if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
            self.pos += 1;
            self.eat_while(digits);
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            let exponent = match self.peek(1) {
                Some(b'+' | b'-') => 2,
                _ => 1,
            };
            if self.peek(exponent).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += exponent;
                self.eat_while(digits);
            }
        }
        // Hexadecimal, octal and binary literals (`0x1F`) and junk glued to a number.
        self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_');
    }

    fn word(&mut self) -> Result<TokenKind, String> {
        if self.postgres() {
            let c = self.bytes[self.pos].to_ascii_lowercase();
            match (c, self.peek(1), self.peek(2)) {
                (b'e', Some(b'\''), _) => {
                    self.pos += 1;
                    self.quoted(b'\'', true)?;
                    return Ok(TokenKind::String);
                }
                (b'b' | b'x' | b'n', Some(b'\''), _) => {
                    self.pos += 1;
                    self.quoted(b'\'', false)?;
                    return Ok(TokenKind::String);
                }
                (b'u', Some(b'&'), Some(quote @ (b'\'' | b'"'))) => {
                    self.pos += 2;
                    self.quoted(quote, false)?;
                    return Ok(if quote == b'"' {
                        TokenKind::QuotedIdent
                    } else {
                        TokenKind::String
                    });
                }
                _ => {}
            }
            self.eat_while(|b| is_ident_char(b) || b == b'$');
        } else {
            self.eat_while(is_ident_char);
        }
        Ok(TokenKind::Word)
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn line_at(src: &str, offset: usize) -> usize {
    1 + count_newlines(&src[..offset])
}

fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'\n').count()
}

/// A statement being accumulated by [`split`].
struct Pending {
    start: usize,
    line: usize,
    comments: Vec<String>,
    /// End of the last token that is neither whitespace nor a comment.
    end: usize,
    /// Nesting of `BEGIN ATOMIC` bodies and the `CASE ... END` expressions inside them.
    atomic_depth: usize,
    atomic_line: usize,
    paren_depth: usize,
    after_begin: bool,
}

impl Pending {
    fn new(token: &Token<'_>, line: usize, comments: Vec<String>) -> Self {
        Self {
            start: token.offset,
            line,
            comments,
            end: token.offset,
            atomic_depth: 0,
            atomic_line: line,
            paren_depth: 0,
            after_begin: false,
        }
    }

    fn top_level(&self) -> bool {
        self.atomic_depth == 0 && self.paren_depth == 0
    }

    fn observe(&mut self, token: &Token<'_>, line: usize, dialect: Dialect) {
        if matches!(
            token.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
        ) {
            return;
        }
        self.end = token.offset + token.text.len();
        if dialect != Dialect::Postgres {
            return;
        }
        let word = |w: &str| token.kind == TokenKind::Word && token.text.eq_ignore_ascii_case(w);
        if word("atomic") && self.after_begin {
            if self.atomic_depth == 0 {
                self.atomic_line = line;
            }
            self.atomic_depth += 1;
        } else if self.atomic_depth > 0 && word("case") {
            self.atomic_depth += 1;
        } else if self.atomic_depth > 0 && word("end") {
            self.atomic_depth -= 1;
        } else if token.kind == TokenKind::Punct && token.text == "(" {
            self.paren_depth += 1;
        } else if token.kind == TokenKind::Punct && token.text == ")" {
            self.paren_depth = self.paren_depth.saturating_sub(1);
        }
        self.after_begin = word("begin");
    }

    fn finish(self, sql: &str, end: usize) -> SplitStatement {
        SplitStatement {
            sql: sql[self.start..end].trim_end().to_owned(),
            comments: self.comments,
            line: self.line,
            offset: self.start,
        }
    }
}

/// Splits on `;` outside strings, quoted identifiers, comments, dollar quotes and (PostgreSQL)
/// parentheses and `BEGIN ATOMIC ... END` bodies. Parentheses are tracked like psql does, so
/// `CREATE RULE ... DO (stmt; stmt)` stays one statement.
///
/// Comments attached to a statement are the `--` lines that each start a line and form a
/// contiguous run ending on the line directly above the statement. A blank line, a block comment
/// or a `#` comment breaks the run; a comment trailing the previous statement on its line is not
/// attached. Comments that are not attached and not inside a statement are dropped. A final
/// statement without `;` ends at its last token, so trailing comments are dropped too.
pub(super) fn split(sql: &str, dialect: Dialect) -> Result<Vec<SplitStatement>, SplitError> {
    let mut statements = Vec::new();
    let mut comments = Vec::new();
    let mut at_line_start = true;
    let mut current: Option<Pending> = None;
    let mut line = 1;

    for token in Lexer::new(sql, dialect) {
        let token = token?;
        let token_line = line;
        let newlines = count_newlines(token.text);
        line += newlines;
        let semicolon = token.kind == TokenKind::Punct && token.text == ";";
        match current.as_mut() {
            Some(stmt) if semicolon && stmt.top_level() => {
                if let Some(stmt) = current.take() {
                    statements.push(stmt.finish(sql, token.offset));
                }
                at_line_start = false;
            }
            Some(stmt) => stmt.observe(&token, token_line, dialect),
            None => match token.kind {
                TokenKind::Whitespace => {
                    if newlines >= 2 {
                        comments.clear();
                    }
                    if newlines >= 1 {
                        at_line_start = true;
                    }
                }
                TokenKind::LineComment if at_line_start && token.text.starts_with("--") => {
                    comments.push(token.text[2..].trim().to_owned());
                    at_line_start = false;
                }
                TokenKind::LineComment | TokenKind::BlockComment => {
                    comments.clear();
                    at_line_start = false;
                }
                _ if semicolon => {
                    comments.clear();
                    at_line_start = false;
                }
                _ => {
                    let mut stmt = Pending::new(&token, token_line, std::mem::take(&mut comments));
                    stmt.observe(&token, token_line, dialect);
                    current = Some(stmt);
                }
            },
        }
    }

    if let Some(stmt) = current {
        if stmt.atomic_depth > 0 {
            return Err(SplitError {
                line: stmt.atomic_line,
                message: "unterminated BEGIN ATOMIC body (missing END)".to_owned(),
            });
        }
        let end = stmt.end;
        statements.push(stmt.finish(sql, end));
    }
    Ok(statements)
}

/// Words and quoted identifiers in order of appearance, duplicates included. Unquoted PostgreSQL
/// words are folded to lower case (ASCII only, like the server); quoted identifiers are unescaped
/// (`U&"..."` escape sequences are kept as written). Lexing stops at the first error.
pub(super) fn identifiers(sql: &str, dialect: Dialect) -> Vec<String> {
    Lexer::new(sql, dialect)
        .map_while(Result::ok)
        .filter_map(|token| match token.kind {
            TokenKind::Word => Some(match dialect {
                Dialect::Postgres => token.text.to_ascii_lowercase(),
                Dialect::ClickHouse => token.text.to_owned(),
            }),
            TokenKind::QuotedIdent => Some(unquote(token.text, dialect)),
            _ => None,
        })
        .collect()
}

fn unquote(text: &str, dialect: Dialect) -> String {
    let text = text
        .strip_prefix("U&")
        .or_else(|| text.strip_prefix("u&"))
        .unwrap_or(text);
    let quote = text.chars().next().unwrap_or('"');
    let inner = text
        .get(1..text.len().saturating_sub(1))
        .unwrap_or_default();
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == quote && chars.peek() == Some(&quote) {
            chars.next();
            out.push(quote);
        } else if c == '\\' && dialect == Dialect::ClickHouse {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('a') => out.push('\u{7}'),
                Some('v') => out.push('\u{b}'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests;
