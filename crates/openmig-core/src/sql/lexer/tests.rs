use super::*;
use crate::sql::{identifiers, split};
use pretty_assertions::assert_eq;

fn pg(sql: &str) -> Vec<String> {
    statements(sql, Dialect::Postgres)
}

fn ch(sql: &str) -> Vec<String> {
    statements(sql, Dialect::ClickHouse)
}

fn statements(sql: &str, dialect: Dialect) -> Vec<String> {
    split(sql, dialect)
        .unwrap_or_else(|e| panic!("split failed: {e}"))
        .into_iter()
        .map(|s| s.sql)
        .collect()
}

fn kinds(sql: &str, dialect: Dialect) -> Vec<(TokenKind, &str)> {
    tokenize(sql, dialect)
        .unwrap()
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .map(|t| (t.kind, t.text))
        .collect()
}

#[test]
fn tokens_cover_the_input() {
    let inputs = [
        "SELECT 'a''b', E'it\\'s', $$x$$, \"q\"\"i\" -- c\r\n/* /* n */ */ 1.5e-3, $1;",
        "SELECT `a\\`b`, 'x\\'y' # c\n-- d\n/* e */ $h$heredoc$h$;",
    ];
    for (sql, dialect) in inputs.iter().zip([Dialect::Postgres, Dialect::ClickHouse]) {
        let tokens = tokenize(sql, dialect).unwrap();
        let joined: String = tokens.iter().map(|t| t.text).collect();
        assert_eq!(&joined, sql);
        let mut offset = 0;
        for t in &tokens {
            assert_eq!(t.offset, offset);
            offset += t.text.len();
        }
    }
}

#[test]
fn postgres_token_kinds() {
    assert_eq!(
        kinds(
            "SELECT E'it\\'s', 'a''b', U&'d\\0061t', B'101', X'1F', N'n', U&\"q\", \"x\"\"y\", $1, $f$ ; $f$, foo$bar, 1.5, .5e3, 0x1F",
            Dialect::Postgres
        ),
        vec![
            (TokenKind::Word, "SELECT"),
            (TokenKind::String, "E'it\\'s'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "'a''b'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "U&'d\\0061t'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "B'101'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "X'1F'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "N'n'"),
            (TokenKind::Punct, ","),
            (TokenKind::QuotedIdent, "U&\"q\""),
            (TokenKind::Punct, ","),
            (TokenKind::QuotedIdent, "\"x\"\"y\""),
            (TokenKind::Punct, ","),
            (TokenKind::Punct, "$1"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "$f$ ; $f$"),
            (TokenKind::Punct, ","),
            (TokenKind::Word, "foo$bar"),
            (TokenKind::Punct, ","),
            (TokenKind::Number, "1.5"),
            (TokenKind::Punct, ","),
            (TokenKind::Number, ".5e3"),
            (TokenKind::Punct, ","),
            (TokenKind::Number, "0x1F"),
        ]
    );
}

#[test]
fn clickhouse_token_kinds() {
    assert_eq!(
        kinds(
            "SELECT 'it\\'s', 'a''b', `x\\`y`, \"z\\\"w\", #tag, $h$a;b$h$ # comment\n#! shebang\n",
            Dialect::ClickHouse
        ),
        vec![
            (TokenKind::Word, "SELECT"),
            (TokenKind::String, "'it\\'s'"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "'a''b'"),
            (TokenKind::Punct, ","),
            (TokenKind::QuotedIdent, "`x\\`y`"),
            (TokenKind::Punct, ","),
            (TokenKind::QuotedIdent, "\"z\\\"w\""),
            (TokenKind::Punct, ","),
            (TokenKind::Punct, "#"),
            (TokenKind::Word, "tag"),
            (TokenKind::Punct, ","),
            (TokenKind::String, "$h$a;b$h$"),
            (TokenKind::LineComment, "# comment"),
            (TokenKind::LineComment, "#! shebang"),
        ]
    );
}

#[test]
fn backslash_is_literal_in_standard_postgres_strings() {
    assert_eq!(
        pg("SELECT 'C:\\'; SELECT 2"),
        vec!["SELECT 'C:\\'", "SELECT 2"]
    );
    assert_eq!(
        pg("SELECT E'\\''; SELECT E'\\\\'; SELECT 3"),
        vec!["SELECT E'\\''", "SELECT E'\\\\'", "SELECT 3"]
    );
}

#[test]
fn dollar_quoted_functions() {
    let sql = r#"
CREATE FUNCTION "app"."touch"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;
CREATE FUNCTION "app"."nested"() RETURNS text LANGUAGE plpgsql AS $body$
DECLARE
  q text := $q$SELECT 1; SELECT 2;$q$;
BEGIN
  EXECUTE q;
  RETURN '$$';
END;
$body$;
DO $$ BEGIN RAISE NOTICE 'hi; there'; END $$;
"#;
    let stmts = pg(sql);
    assert_eq!(stmts.len(), 3);
    assert!(stmts[0].starts_with("CREATE FUNCTION \"app\".\"touch\""));
    assert!(stmts[0].ends_with("END;\n$$"));
    assert!(stmts[1].ends_with("END;\n$body$"));
    assert_eq!(stmts[2], "DO $$ BEGIN RAISE NOTICE 'hi; there'; END $$");
}

#[test]
fn positional_parameters_are_not_dollar_quotes() {
    assert_eq!(
        pg("PREPARE p AS SELECT $1 + $2; EXECUTE p(1, 2)"),
        vec!["PREPARE p AS SELECT $1 + $2", "EXECUTE p(1, 2)"]
    );
}

#[test]
fn nested_block_comments() {
    assert_eq!(
        pg("SELECT 1 /* outer /* inner; */ still; */; SELECT 2;"),
        vec!["SELECT 1 /* outer /* inner; */ still; */", "SELECT 2"]
    );
    assert_eq!(
        ch("SELECT 1 /* a /* b */; SELECT 2;"),
        vec!["SELECT 1 /* a /* b */", "SELECT 2"]
    );
}

#[test]
fn begin_atomic_bodies() {
    let sql = "\
CREATE FUNCTION add(a integer, b integer) RETURNS integer LANGUAGE sql
BEGIN ATOMIC
  SELECT CASE WHEN a IS NULL THEN 0 ELSE a END + b;
  SELECT CASE b WHEN 1 THEN (SELECT 1) ELSE 2 END;
END;
CREATE PROCEDURE p() LANGUAGE sql begin atomic insert into t values (1); end;
BEGIN;
SELECT 1;
COMMIT;
";
    let stmts = pg(sql);
    assert_eq!(stmts.len(), 5, "{stmts:#?}");
    assert!(stmts[0].starts_with("CREATE FUNCTION add"));
    assert!(stmts[0].ends_with("ELSE 2 END;\nEND"));
    assert_eq!(
        stmts[1],
        "CREATE PROCEDURE p() LANGUAGE sql begin atomic insert into t values (1); end"
    );
    assert_eq!(&stmts[2..], ["BEGIN", "SELECT 1", "COMMIT"]);
}

#[test]
fn unterminated_begin_atomic_is_an_error() {
    let err = split(
        "SELECT 1;\nCREATE FUNCTION f() RETURNS int LANGUAGE sql\nBEGIN ATOMIC SELECT 1;",
        Dialect::Postgres,
    )
    .unwrap_err();
    assert_eq!(err.line, 3);
    assert!(err.message.contains("BEGIN ATOMIC"));
}

#[test]
fn parentheses_keep_rule_actions_together() {
    assert_eq!(
        pg(
            "CREATE RULE r AS ON INSERT TO t DO ALSO (INSERT INTO a VALUES (1); INSERT INTO b VALUES (2)); SELECT 1"
        ),
        vec![
            "CREATE RULE r AS ON INSERT TO t DO ALSO (INSERT INTO a VALUES (1); INSERT INTO b VALUES (2))",
            "SELECT 1"
        ]
    );
}

#[test]
fn create_trigger() {
    let sql = r#"-- Create trigger "set_updated_at"
CREATE TRIGGER "set_updated_at" BEFORE UPDATE ON "app"."users" FOR EACH ROW EXECUTE FUNCTION "app"."touch"();
-- Create policy "own_rows"
CREATE POLICY "own_rows" ON "app"."users" AS PERMISSIVE FOR SELECT TO "api" USING ((owner = current_setting('app.user_id'::text)));
"#;
    let stmts = split(sql, Dialect::Postgres).unwrap();
    assert_eq!(stmts.len(), 2);
    assert_eq!(stmts[0].comments, vec!["Create trigger \"set_updated_at\""]);
    assert_eq!(stmts[0].line, 2);
    assert!(stmts[0].sql.ends_with("\"app\".\"touch\"()"));
    assert_eq!(stmts[1].comments, vec!["Create policy \"own_rows\""]);
    assert_eq!(stmts[1].line, 4);
}

#[test]
fn multiple_statements_per_line_and_empty_statements() {
    let stmts = split("SELECT 1; SELECT 2;; ;\nSELECT 3", Dialect::Postgres).unwrap();
    let got: Vec<_> = stmts
        .iter()
        .map(|s| (s.sql.as_str(), s.line, s.offset))
        .collect();
    assert_eq!(
        got,
        vec![("SELECT 1", 1, 0), ("SELECT 2", 1, 10), ("SELECT 3", 2, 23)]
    );
}

#[test]
fn comment_attachment() {
    let sql = "\
-- openmig:generated

-- first
-- second
SELECT 1; -- trailing
-- third
SELECT 2 -- inside
  + 3;

-- detached

SELECT 4;
/* block */
SELECT 5;
-- before block
/* block */
SELECT 6;
-- only a comment at the end
";
    let stmts = split(sql, Dialect::Postgres).unwrap();
    let got: Vec<_> = stmts
        .iter()
        .map(|s| (s.sql.as_str(), s.comments.clone(), s.line))
        .collect();
    let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        got,
        vec![
            ("SELECT 1", strings(&["first", "second"]), 5),
            ("SELECT 2 -- inside\n  + 3", strings(&["third"]), 7),
            ("SELECT 4", vec![], 12),
            ("SELECT 5", vec![], 14),
            ("SELECT 6", vec![], 17),
        ]
    );
}

#[test]
fn comments_inside_statements_are_kept() {
    assert_eq!(
        pg("SELECT 1 /* ; */ -- ;\n, 2;"),
        vec!["SELECT 1 /* ; */ -- ;\n, 2"]
    );
}

#[test]
fn final_statement_without_semicolon() {
    let stmts = split(
        "SELECT 1;\n-- c\nSELECT 2\n-- trailing\n",
        Dialect::Postgres,
    )
    .unwrap();
    assert_eq!(stmts.len(), 2);
    assert_eq!(stmts[1].sql, "SELECT 2");
    assert_eq!(stmts[1].comments, vec!["c"]);
}

#[test]
fn crlf_line_endings() {
    let sql = "-- one\r\nCREATE TABLE t (\r\n  a int\r\n);\r\n\r\n-- two\r\nDROP TABLE t;\r\n";
    let stmts = split(sql, Dialect::Postgres).unwrap();
    assert_eq!(stmts.len(), 2);
    assert_eq!(stmts[0].comments, vec!["one"]);
    assert_eq!(stmts[0].sql, "CREATE TABLE t (\r\n  a int\r\n)");
    assert_eq!(stmts[0].line, 2);
    assert_eq!(stmts[1].comments, vec!["two"]);
    assert_eq!(stmts[1].sql, "DROP TABLE t");
    assert_eq!(stmts[1].line, 7);
    assert_eq!(&sql[stmts[1].offset..stmts[1].offset + 4], "DROP");
}

#[test]
fn offsets_point_at_statement_text() {
    let sql = "-- a\nSELECT 'é';\n  -- b\n  SELECT \"ü\";";
    for s in split(sql, Dialect::Postgres).unwrap() {
        assert!(sql[s.offset..].starts_with(&s.sql));
    }
}

#[test]
fn unterminated_tokens_are_errors() {
    let cases = [
        ("SELECT 1;\nSELECT 'abc", Dialect::Postgres, 2, "string"),
        ("SELECT \"abc", Dialect::Postgres, 1, "quoted identifier"),
        (
            "SELECT 1;\n\n/* a /* b */",
            Dialect::Postgres,
            3,
            "block comment",
        ),
        (
            "CREATE FUNCTION f() AS $body$\nSELECT 1;\n$$;",
            Dialect::Postgres,
            1,
            "$body$",
        ),
        ("SELECT E'it\\'s", Dialect::Postgres, 1, "string"),
        ("SELECT `abc", Dialect::ClickHouse, 1, "quoted identifier"),
        ("SELECT 'it\\'s", Dialect::ClickHouse, 1, "string"),
        ("SELECT 1 /* x", Dialect::ClickHouse, 1, "block comment"),
        ("SELECT $h$ x", Dialect::ClickHouse, 1, "heredoc"),
    ];
    for (sql, dialect, line, needle) in cases {
        let err = split(sql, dialect).unwrap_err();
        assert_eq!(err.line, line, "{sql}");
        assert!(err.message.contains(needle), "{sql}: {}", err.message);
    }
}

#[test]
fn clickhouse_statements() {
    let sql = r#"
-- Create "events" table
CREATE TABLE `dialo`.`events` (
  `id` String COMMENT 'it\'s; fine',
  `ts` DateTime64(9) DEFAULT now64() -- a comment; with a semicolon
) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')
ORDER BY (`ts`);
# hash comment; ignored
#! shebang; ignored
INSERT INTO t SELECT * FROM s WHERE x = $heredoc$;$heredoc$; SELECT "a\"b";
"#;
    let stmts = split(sql, Dialect::ClickHouse).unwrap();
    assert_eq!(stmts.len(), 3);
    assert_eq!(stmts[0].comments, vec!["Create \"events\" table"]);
    assert!(stmts[0].sql.ends_with("ORDER BY (`ts`)"));
    assert_eq!(
        stmts[1].sql,
        "INSERT INTO t SELECT * FROM s WHERE x = $heredoc$;$heredoc$"
    );
    assert!(stmts[1].comments.is_empty());
    assert_eq!(stmts[2].sql, "SELECT \"a\\\"b\"");
}

#[test]
fn postgres_hash_is_an_operator() {
    assert_eq!(
        pg("SELECT 5 # 3; SELECT 1"),
        vec!["SELECT 5 # 3", "SELECT 1"]
    );
}

#[test]
fn identifiers_postgres() {
    assert_eq!(
        identifiers(
            "SELECT Foo, \"Bar\", \"a\"\"b\", 'str', $$ body $$ -- cmt\nFROM app.Users /* x */ JOIN app.users",
            Dialect::Postgres
        ),
        vec![
            "select", "foo", "Bar", "a\"b", "from", "app", "users", "join", "app", "users"
        ]
    );
}

#[test]
fn identifiers_clickhouse() {
    assert_eq!(
        identifiers(
            "SELECT Foo, `Bar`, `a\\`b`, \"c\"\"d\", 'str' # cmt\nFROM db.Events",
            Dialect::ClickHouse
        ),
        vec![
            "SELECT", "Foo", "Bar", "a`b", "c\"d", "FROM", "db", "Events"
        ]
    );
}

#[test]
fn identifiers_stop_at_lex_errors() {
    assert_eq!(
        identifiers("SELECT a, 'unterminated b", Dialect::Postgres),
        vec!["select", "a"]
    );
}

fn first_word(sql: &str, dialect: Dialect) -> String {
    tokenize(sql, dialect)
        .unwrap()
        .into_iter()
        .find(|t| t.kind == TokenKind::Word)
        .map(|t| t.text.to_ascii_uppercase())
        .unwrap_or_default()
}

/// Splits every real migration file of the dialo repository (when checked out next to this one)
/// and checks that every statement starts with a statement keyword.
#[test]
#[ignore = "needs /home/coder/dialo/dialo-db"]
fn dialo_corpus() {
    let root = std::path::Path::new("/home/coder/dialo/dialo-db");
    let corpora = [
        (
            "pg",
            Dialect::Postgres,
            &[
                "ALTER", "COMMENT", "CREATE", "DELETE", "DO", "DROP", "GRANT", "INSERT", "REVOKE",
                "SELECT", "SET", "TRUNCATE", "UPDATE", "WITH", "REFRESH", "REINDEX", "ANALYZE",
                "VACUUM",
            ][..],
        ),
        (
            "ch",
            Dialect::ClickHouse,
            &[
                "ALTER", "CREATE", "DROP", "INSERT", "RENAME", "SELECT", "SET", "TRUNCATE",
                "OPTIMIZE", "SYSTEM", "EXCHANGE", "GRANT", "REVOKE",
            ][..],
        ),
    ];
    let mut checked = 0;
    for (sub, dialect, keywords) in corpora {
        let dir = root.join(sub).join("migrations");
        if !dir.is_dir() {
            continue;
        }
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sql"))
            .collect();
        paths.sort();
        let (mut files, mut total, mut uncommented, mut dollar, mut max) = (0, 0, 0, 0, 0);
        let mut first_words = std::collections::BTreeMap::<String, usize>::new();
        for path in &paths {
            let content = std::fs::read_to_string(path).unwrap();
            let stmts =
                split(&content, dialect).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            files += 1;
            total += stmts.len();
            max = max.max(stmts.len());
            for s in &stmts {
                assert!(
                    content[s.offset..].starts_with(&s.sql),
                    "{}:{}",
                    path.display(),
                    s.line
                );
                let word = first_word(&s.sql, dialect);
                assert!(
                    keywords.contains(&word.as_str()),
                    "{}:{}: statement starts with {word:?}:\n{}",
                    path.display(),
                    s.line,
                    s.sql
                );
                *first_words.entry(word).or_default() += 1;
                if s.comments.is_empty() {
                    uncommented += 1;
                }
                if s.sql.contains('$') {
                    dollar += 1;
                }
                let semicolons = tokenize(&s.sql, dialect)
                    .unwrap()
                    .iter()
                    .filter(|t| t.kind == TokenKind::Punct && t.text == ";")
                    .count();
                assert!(
                    semicolons == 0 || s.sql.to_ascii_uppercase().contains("BEGIN ATOMIC"),
                    "{}:{}: top-level `;` inside a statement",
                    path.display(),
                    s.line
                );
            }
        }
        checked += files;
        println!(
            "{sub}: {files} files, {total} statements (max {max} per file), {dollar} with dollar quotes, {uncommented} without a comment line, first words: {first_words:?}"
        );
    }
    assert!(checked > 0, "no dialo migration files found");
}
