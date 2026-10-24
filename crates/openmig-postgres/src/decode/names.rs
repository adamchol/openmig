//! Identifier quoting and the names PostgreSQL chooses for unnamed objects.

use openmig_core::sql::{Dialect, Token, TokenKind, tokenize};

/// PostgreSQL's keywords that are not unreserved (`pg_get_keywords()` with `catcode <> 'U'`,
/// PostgreSQL 17). `quote_ident` quotes these.
const KEYWORDS: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "between",
    "bigint",
    "binary",
    "bit",
    "boolean",
    "both",
    "case",
    "cast",
    "char",
    "character",
    "check",
    "coalesce",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "dec",
    "decimal",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "exists",
    "extract",
    "false",
    "fetch",
    "float",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "greatest",
    "group",
    "grouping",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "inout",
    "int",
    "integer",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "join",
    "json",
    "json_array",
    "json_arrayagg",
    "json_exists",
    "json_object",
    "json_objectagg",
    "json_query",
    "json_scalar",
    "json_serialize",
    "json_table",
    "json_value",
    "lateral",
    "leading",
    "least",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "merge_action",
    "national",
    "natural",
    "nchar",
    "none",
    "normalize",
    "not",
    "notnull",
    "null",
    "nullif",
    "numeric",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "out",
    "outer",
    "overlaps",
    "overlay",
    "placing",
    "position",
    "precision",
    "primary",
    "real",
    "references",
    "returning",
    "right",
    "row",
    "select",
    "session_user",
    "setof",
    "similar",
    "smallint",
    "some",
    "substring",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "time",
    "timestamp",
    "to",
    "trailing",
    "treat",
    "trim",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "values",
    "varchar",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
    "xmlattributes",
    "xmlconcat",
    "xmlelement",
    "xmlexists",
    "xmlforest",
    "xmlnamespaces",
    "xmlparse",
    "xmlpi",
    "xmlroot",
    "xmlserialize",
    "xmltable",
];

/// Maximum identifier length in bytes (`NAMEDATALEN - 1`).
pub(crate) const MAX_IDENT: usize = 63;

/// Whether `name` can be written without quotes, by the rule of PostgreSQL's `quote_ident`:
/// lower-case letters, digits and underscores, not starting with a digit, and not a keyword
/// other than an unreserved one.
pub(crate) fn is_plain_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && KEYWORDS.binary_search(&name).is_err()
}

/// `name` as PostgreSQL's `quote_ident` prints it.
pub(crate) fn quote_ident(name: &str) -> String {
    if is_plain_ident(name) {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// `schema.name`, each part quoted only when needed.
pub(crate) fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

/// Splits SQL text that names an object (`app.status`, `"Odd"."x.y"`, `text`) into its unquoted
/// parts. `None` when the text is anything else (an array type, a type with modifiers, ...).
pub(crate) fn split_name(text: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut rest = text;
    loop {
        let (part, after) = if let Some(quoted) = rest.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let end = loop {
                let (i, c) = chars.next()?;
                if c == '"' {
                    if quoted[i + 1..].starts_with('"') {
                        value.push('"');
                        chars.next();
                    } else {
                        break i + 1;
                    }
                } else {
                    value.push(c);
                }
            };
            (value, &quoted[end..])
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            if !is_plain_ident(word) {
                return None;
            }
            (word.to_owned(), &rest[end..])
        };
        parts.push(part);
        match after.strip_prefix('.') {
            Some(next) => rest = next,
            None if after.is_empty() => return Some(parts),
            None => return None,
        }
    }
}

/// The text of a single identifier as SQL prints it (`"C"`, `text_pattern_ops`), unquoted.
pub(crate) fn unquote_single(text: &str) -> Option<String> {
    match split_name(text)?.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Truncates `s` to at most `max` bytes on a character boundary.
fn clip(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// PostgreSQL's `makeObjectName`: `name1[_name2][_label]`, shortening the longer of the two names
/// until the result fits in [`MAX_IDENT`] bytes.
pub(crate) fn make_object_name(name1: &str, name2: Option<&str>, label: &str) -> String {
    let mut overhead = 0;
    if name2.is_some() {
        overhead += 1;
    }
    if !label.is_empty() {
        overhead += label.len() + 1;
    }
    let avail = MAX_IDENT.saturating_sub(overhead);
    let mut n1 = name1.len();
    let mut n2 = name2.map_or(0, str::len);
    while n1 + n2 > avail {
        if n1 > n2 {
            n1 -= 1;
        } else {
            n2 -= 1;
        }
    }
    let mut out = clip(name1, n1).to_owned();
    if let Some(name2) = name2 {
        out.push('_');
        out.push_str(clip(name2, n2));
    }
    if !label.is_empty() {
        out.push('_');
        out.push_str(label);
    }
    out
}

/// PostgreSQL's `ChooseRelationName`: [`make_object_name`], with a number appended to the label
/// while the name is `taken`.
pub(crate) fn choose_name(
    name1: &str,
    name2: Option<&str>,
    label: &str,
    taken: impl Fn(&str) -> bool,
) -> String {
    let mut name = make_object_name(name1, name2, label);
    let mut n = 0;
    while taken(&name) {
        n += 1;
        name = make_object_name(name1, name2, &format!("{label}{n}"));
    }
    name
}

/// PostgreSQL's `ChooseIndexNameAddition` over the key column names: the names joined with `_`,
/// stopping once the result reaches the identifier length limit.
pub(crate) fn index_name_addition(names: &[String]) -> String {
    let mut out = String::new();
    for name in names {
        if !out.is_empty() {
            out.push('_');
        }
        out.push_str(clip(name, MAX_IDENT));
        if out.len() > MAX_IDENT {
            break;
        }
    }
    out
}

/// PostgreSQL's `ChooseIndexColumnNames`: key names made unique by appending a number.
pub(crate) fn index_column_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for orig in names {
        let mut name = orig.clone();
        let mut n = 0;
        while out.contains(&name) {
            n += 1;
            let suffix = n.to_string();
            name = format!(
                "{}{suffix}",
                clip(&orig, MAX_IDENT.saturating_sub(suffix.len()))
            );
        }
        out.push(name);
    }
    out
}

/// The name PostgreSQL derives from an index expression (`FigureIndexColname`): the function
/// name of a call, the column of a column reference (also under a cast), `expr` otherwise.
pub(crate) fn expression_column_name(expr: &str) -> String {
    let Ok(tokens) = tokenize(expr, Dialect::Postgres) else {
        return "expr".into();
    };
    let tokens: Vec<&Token<'_>> = tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
            )
        })
        .collect();
    let mut tokens = tokens.as_slice();
    while tokens.len() >= 2
        && tokens[0].text == "("
        && tokens[tokens.len() - 1].text == ")"
        && closes_at(tokens) == Some(tokens.len() - 1)
    {
        tokens = &tokens[1..tokens.len() - 1];
    }
    let name_of = |t: &Token<'_>| match t.kind {
        TokenKind::Word => Some(t.text.to_lowercase()),
        TokenKind::QuotedIdent => unquote_single(t.text),
        _ => None,
    };
    let mut i = 0;
    let mut last = None;
    while let Some(name) = tokens.get(i).and_then(|t| name_of(t)) {
        last = Some(name);
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text == ".") {
            i += 1;
        } else {
            break;
        }
    }
    let Some(name) = last else {
        return "expr".into();
    };
    let rest = &tokens[i..];
    match rest.first().map(|t| t.text) {
        None => name,
        Some("(") if closes_at(rest) == Some(rest.len() - 1) => name,
        Some(":") if rest.get(1).is_some_and(|t| t.text == ":") => name,
        _ => "expr".into(),
    }
}

/// Index of the bracket that closes `tokens[0]`.
fn closes_at(tokens: &[&Token<'_>]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        match t.text {
            "(" | "[" => depth += 1,
            ")" | "]" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_sorted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("users"), "users");
        assert_eq!(quote_ident("user"), "\"user\"");
        assert_eq!(quote_ident("Users"), "\"Users\"");
        assert_eq!(quote_ident("a$b"), "\"a$b\"");
        assert_eq!(quote_ident("2fa"), "\"2fa\"");
        assert_eq!(quote_ident("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(quote_ident("current_role"), "\"current_role\"");
        assert_eq!(qualified("app", "integer"), "app.\"integer\"");
    }

    #[test]
    fn splitting_names() {
        assert_eq!(
            split_name("app.status"),
            Some(vec!["app".into(), "status".into()])
        );
        assert_eq!(
            split_name("\"Odd\".\"x.y\"\"z\""),
            Some(vec!["Odd".into(), "x.y\"z".into()])
        );
        assert_eq!(
            split_name("app.\"integer\""),
            Some(vec!["app".into(), "integer".into()])
        );
        assert_eq!(split_name("text[]"), None);
        assert_eq!(split_name("character varying(10)"), None);
        assert_eq!(split_name("integer"), None);
        assert_eq!(unquote_single("\"C\""), Some("C".into()));
        assert_eq!(
            unquote_single("text_pattern_ops"),
            Some("text_pattern_ops".into())
        );
    }

    #[test]
    fn object_names() {
        assert_eq!(make_object_name("users", None, "pkey"), "users_pkey");
        assert_eq!(
            make_object_name("users", Some("org_id_email"), "idx"),
            "users_org_id_email_idx"
        );
        let long = "a".repeat(70);
        let name = make_object_name(&long, Some("id"), "seq");
        assert_eq!(name.len(), 63);
        assert!(name.ends_with("a_id_seq"));
        let both = make_object_name(&"t".repeat(40), Some(&"c".repeat(40)), "idx");
        assert_eq!(both, format!("{}_{}_idx", "t".repeat(29), "c".repeat(29)));
        assert_eq!(
            choose_name("t", Some("a"), "idx", |n| n == "t_a_idx"),
            "t_a_idx1"
        );
        assert_eq!(
            index_column_names(["a".to_owned(), "a".to_owned(), "b".to_owned()]),
            ["a", "a1", "b"]
        );
    }

    #[test]
    fn expression_names() {
        assert_eq!(expression_column_name("lower(email)"), "lower");
        assert_eq!(expression_column_name("lower(email::text)"), "lower");
        assert_eq!(expression_column_name("(lower(email))"), "lower");
        assert_eq!(expression_column_name("public.f(a, b)"), "f");
        assert_eq!(expression_column_name("email::text"), "email");
        assert_eq!(expression_column_name("(score + 1)"), "expr");
        assert_eq!(expression_column_name("lower(a) || b"), "expr");
        assert_eq!(expression_column_name("\"Name\""), "Name");
    }
}
