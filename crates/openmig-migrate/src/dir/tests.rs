use super::*;
use openmig_core::sql::{Dialect, split};
use openmig_core::{Statement, TxRequirement};
use pretty_assertions::assert_eq;

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

fn migrations_error(result: Result<MigrationDir, Error>) -> String {
    match result {
        Err(Error::Migrations(message)) => message,
        other => panic!("expected a migrations error, got {other:?}"),
    }
}

#[test]
fn file_names() {
    assert_eq!(
        parse_file_name("20261024120000_add_users.sql").unwrap(),
        ("20261024120000".to_owned(), "add_users".to_owned())
    );
    assert_eq!(
        parse_file_name("20261024120000.sql").unwrap(),
        ("20261024120000".to_owned(), String::new())
    );
    assert_eq!(
        parse_file_name("20261024120000_v2_fix_9.sql").unwrap().1,
        "v2_fix_9"
    );
    let cases = [
        ("20261024120000_add_users.txt", "must end in `.sql`"),
        ("2026102412000.sql", "14-digit UTC timestamp"),
        ("add_users.sql", "14-digit UTC timestamp"),
        ("202610241200001.sql", "followed by `_<name>` or `.sql`"),
        ("20261024120000-add.sql", "followed by `_<name>` or `.sql`"),
        ("20261024120000_.sql", "name after `_` is empty"),
        (
            "20261024120000_AddUsers.sql",
            "lowercase letters, digits and `_`",
        ),
        (
            "20261024120000_add-users.sql",
            "lowercase letters, digits and `_`",
        ),
    ];
    for (file, reason) in cases {
        let err = parse_file_name(file).unwrap_err();
        assert_eq!(err.file, file);
        assert!(err.reason.contains(reason), "{file}: {}", err.reason);
        assert!(
            err.to_string()
                .starts_with(&format!("invalid migration file name `{file}`"))
        );
    }
    assert_eq!(file_name("20261024120000", ""), "20261024120000.sql");
    assert_eq!(file_name("20261024120000", "x"), "20261024120000_x.sql");
}

#[test]
fn load_sorts_and_ignores_other_files() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "20260102000000_b.sql",
        "-- openmig:no-transaction\nSELECT 2;\n",
    );
    write(tmp.path(), "20260101000000.sql", "SELECT 1;\n");
    write(tmp.path(), "README.md", "not a migration");
    write(tmp.path(), "atlas.sum", "h1:...");
    std::fs::create_dir(tmp.path().join("nested")).unwrap();
    write(&tmp.path().join("nested"), "bogus.sql", "SELECT 3;");

    let dir = MigrationDir::load(tmp.path()).unwrap();
    let names: Vec<_> = dir.files.iter().map(|f| f.file_name.as_str()).collect();
    assert_eq!(names, ["20260101000000.sql", "20260102000000_b.sql"]);
    let b = &dir.files[1];
    assert_eq!(b.version, "20260102000000");
    assert_eq!(b.name, "b");
    assert_eq!(b.path, tmp.path().join("20260102000000_b.sql"));
    assert_eq!(
        b.checksum,
        sha256_hex(b"-- openmig:no-transaction\nSELECT 2;\n")
    );
    assert!(b.directives.no_transaction);
    assert_eq!(dir.latest_version(), Some("20260102000000"));
}

#[test]
fn load_missing_directory_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = MigrationDir::load(&tmp.path().join("missing")).unwrap();
    assert!(dir.files.is_empty());
    assert_eq!(dir.verify_sum(), Ok(()));
}

#[test]
fn load_errors_name_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "20260101000000_Bad.sql", "SELECT 1;");
    let message = migrations_error(MigrationDir::load(tmp.path()));
    assert!(message.contains("20260101000000_Bad.sql"), "{message}");
    assert!(message.contains("invalid migration file name"), "{message}");

    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "20260101000000_a.sql", "SELECT 1;");
    write(tmp.path(), "20260101000000_b.sql", "SELECT 1;");
    let message = migrations_error(MigrationDir::load(tmp.path()));
    assert!(
        message.contains("duplicate migration version 20260101000000: 20260101000000_a.sql and 20260101000000_b.sql"),
        "{message}"
    );

    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "20260101000000_a.sql",
        "-- openmig:nope\nSELECT 1;",
    );
    let message = migrations_error(MigrationDir::load(tmp.path()));
    assert!(
        message.contains("20260101000000_a.sql: line 1: unknown directive"),
        "{message}"
    );

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("20260101000000_a.sql"), [0xff, 0xfe]).unwrap();
    let message = migrations_error(MigrationDir::load(tmp.path()));
    assert!(message.contains("not valid UTF-8"), "{message}");
}

fn sum_dir(files: &[(&str, &str)]) -> (tempfile::TempDir, MigrationDir) {
    let tmp = tempfile::tempdir().unwrap();
    for (name, content) in files {
        write(tmp.path(), name, content);
    }
    let dir = MigrationDir::load(tmp.path()).unwrap();
    dir.write_sum().unwrap();
    (tmp, dir)
}

#[test]
fn sum_round_trip() {
    let (tmp, dir) = sum_dir(&[
        ("20260101000000_a.sql", "SELECT 1;\n"),
        ("20260102000000.sql", "SELECT 2;\n"),
    ]);
    let text = std::fs::read_to_string(tmp.path().join(SUM_FILE_NAME)).unwrap();
    let body = format!(
        "20260101000000_a.sql {}\n20260102000000.sql {}\n",
        sha256_hex(b"SELECT 1;\n"),
        sha256_hex(b"SELECT 2;\n")
    );
    assert_eq!(
        text,
        format!(
            "openmig-sum 1 total:{}\n{body}",
            sha256_hex(body.as_bytes())
        )
    );
    let parsed = SumFile::parse(&text).unwrap();
    assert_eq!(parsed, dir.sum());
    assert_eq!(parsed.render(), text);
    assert_eq!(SumFile::read(tmp.path()).unwrap(), Some(parsed));
    assert_eq!(dir.verify_sum(), Ok(()));
    assert_eq!(
        SumFile::parse(&text.replace('\n', "\r\n")).unwrap(),
        dir.sum()
    );
}

#[test]
fn empty_sum() {
    let text = SumFile::default().render();
    assert_eq!(text, format!("openmig-sum 1 total:{}\n", sha256_hex(b"")));
    assert_eq!(SumFile::parse(&text).unwrap(), SumFile::default());
}

#[test]
fn sum_detects_added_removed_and_edited_files() {
    let (tmp, _) = sum_dir(&[
        ("20260101000000_a.sql", "SELECT 1;\n"),
        ("20260102000000_b.sql", "SELECT 2;\n"),
    ]);
    write(tmp.path(), "20260101000000_a.sql", "SELECT 10;\n");
    std::fs::remove_file(tmp.path().join("20260102000000_b.sql")).unwrap();
    write(tmp.path(), "20260103000000_c.sql", "SELECT 3;\n");
    let dir = MigrationDir::load(tmp.path()).unwrap();
    let err = dir.verify_sum().unwrap_err();
    assert_eq!(
        err,
        SumError::Mismatch {
            added: vec!["20260103000000_c.sql".to_owned()],
            removed: vec!["20260102000000_b.sql".to_owned()],
            edited: vec!["20260101000000_a.sql".to_owned()],
        }
    );
    assert_eq!(
        err.to_string(),
        "migration files do not match openmig.sum:\n  added: 20260103000000_c.sql\n  removed: 20260102000000_b.sql\n  edited: 20260101000000_a.sql"
    );
    dir.write_sum().unwrap();
    assert_eq!(MigrationDir::load(tmp.path()).unwrap().verify_sum(), Ok(()));
}

#[test]
fn sum_missing_only_matters_with_files() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(MigrationDir::load(tmp.path()).unwrap().verify_sum(), Ok(()));
    write(tmp.path(), "20260101000000.sql", "SELECT 1;");
    assert_eq!(
        MigrationDir::load(tmp.path()).unwrap().verify_sum(),
        Err(SumError::Missing)
    );
    assert_eq!(SumFile::read(tmp.path()).unwrap(), None);
}

#[test]
fn sum_total_mismatch() {
    let (tmp, dir) = sum_dir(&[("20260101000000_a.sql", "SELECT 1;\n")]);
    let text = std::fs::read_to_string(tmp.path().join(SUM_FILE_NAME)).unwrap();
    let forged = text.replace(&dir.files[0].checksum, &sha256_hex(b"other"));
    write(tmp.path(), SUM_FILE_NAME, &forged);
    assert_eq!(dir.verify_sum(), Err(SumError::TotalMismatch));
    let err = SumFile::read(tmp.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn sum_malformed_lines() {
    let hash = sha256_hex(b"x");
    let header = |body: &str| {
        format!(
            "openmig-sum 1 total:{}\n{body}",
            sha256_hex(body.as_bytes())
        )
    };
    let cases = [
        (
            String::new(),
            1,
            "expected `openmig-sum 1 total:<sha256 hex>`",
        ),
        ("atlas-sum 1 total:abc\n".to_owned(), 1, "expected"),
        (
            format!("openmig-sum 2 total:{hash}\n"),
            1,
            "unsupported format version `2`",
        ),
        ("openmig-sum 1 total:xyz\n".to_owned(), 1, "expected"),
        (format!("openmig-sum 1 total:{hash} extra\n"), 1, "expected"),
        (
            header(&format!("a.sql {hash}\nbroken\n")),
            3,
            "expected `<file name> <sha256 hex>`",
        ),
        (
            header(&format!("a.sql {hash}\n\n")),
            3,
            "expected `<file name> <sha256 hex>`",
        ),
        (header("a.sql ABC\n"), 2, "not a lowercase sha256"),
        (
            header(&format!("b.sql {hash}\na.sql {hash}\n")),
            3,
            "not sorted",
        ),
        (
            header(&format!("a.sql {hash}\na.sql {hash}\n")),
            3,
            "duplicate entry for `a.sql`",
        ),
        (
            header(&format!("a.sql  {hash}\n")),
            2,
            "not a lowercase sha256",
        ),
    ];
    for (text, line, needle) in cases {
        match SumFile::parse(&text) {
            Err(SumError::Malformed { line: got, message }) => {
                assert_eq!(got, line, "{text:?}: {message}");
                assert!(message.contains(needle), "{text:?}: {message}");
            }
            other => panic!("{text:?}: expected malformed, got {other:?}"),
        }
    }
}

#[test]
fn next_version() {
    let tmp = tempfile::tempdir().unwrap();
    let mut dir = MigrationDir::load(tmp.path()).unwrap();
    let now: jiff::Timestamp = "2026-10-24T12:00:00Z".parse().unwrap();
    assert_eq!(dir.next_version(now), "20261024120000");

    dir.add("20261024115959", "", "SELECT 1;").unwrap();
    assert_eq!(dir.next_version(now), "20261024120000");

    dir.add("20261024120000", "", "SELECT 1;").unwrap();
    assert_eq!(dir.next_version(now), "20261024120001");

    dir.add("20261231235959", "future", "SELECT 1;").unwrap();
    assert_eq!(dir.next_version(now), "20270101000000");

    dir.add("99999999999999", "", "SELECT 1;").unwrap();
    assert_eq!(dir.next_version(now), "100000000000000");
}

#[test]
fn add_and_remove_rewrite_the_sum() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("migrations");
    let mut dir = MigrationDir::load(&path).unwrap();
    let added = dir
        .add(
            "20260102000000",
            "second",
            "-- openmig:no-transaction\nSELECT 2;\n",
        )
        .unwrap();
    assert_eq!(added.file_name, "20260102000000_second.sql");
    assert!(added.directives.no_transaction);
    dir.add("20260101000000", "first", "SELECT 1;\n").unwrap();
    assert_eq!(
        dir.files
            .iter()
            .map(|f| f.version.as_str())
            .collect::<Vec<_>>(),
        ["20260101000000", "20260102000000"]
    );
    assert_eq!(MigrationDir::load(&path).unwrap(), dir);
    assert_eq!(dir.verify_sum(), Ok(()));

    match dir.add("20260101000000", "again", "SELECT 1;") {
        Err(Error::Migrations(m)) => assert!(
            m.contains("already used by 20260101000000_first.sql"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        dir.add("20260103000000", "Bad", ""),
        Err(Error::Migrations(_))
    ));
    assert!(matches!(
        dir.add("20260103000000", "x", "-- openmig:bogus\n"),
        Err(Error::Migrations(_))
    ));
    assert!(!path.join("20260103000000_x.sql").exists());

    dir.remove("20260101000000_first.sql").unwrap();
    assert!(!path.join("20260101000000_first.sql").exists());
    let reloaded = MigrationDir::load(&path).unwrap();
    assert_eq!(reloaded, dir);
    assert_eq!(reloaded.verify_sum(), Ok(()));
    assert!(matches!(
        dir.remove("20260101000000_first.sql"),
        Err(Error::Migrations(_))
    ));
}

fn plan() -> Plan {
    let mut plan = Plan::default();
    plan.push(Statement::new(
        "CREATE TABLE \"app\".\"users\" (\n  \"id\" bigint NOT NULL,\n  \"note\" text DEFAULT 'a;b'\n)",
        "Create table \"app\".\"users\"",
    ));
    plan.push(Statement::new(
        "CREATE FUNCTION \"app\".\"f\"() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN NEW;\nEND;\n$$",
        "Create function \"app\".\"f\"",
    ));
    plan.push(Statement::new(
        "CREATE FUNCTION \"app\".\"g\"() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  SELECT CASE WHEN true THEN 1 ELSE 2 END;\nEND",
        "",
    ));
    plan.push(Statement::new(
        "COMMENT ON TABLE \"app\".\"users\" IS 'x'",
        "multi\nline",
    ));
    plan
}

#[test]
fn render_file_layout() {
    let mut plan = Plan::default();
    plan.push(
        Statement::new("DROP INDEX CONCURRENTLY \"app\".\"i\"", "Drop index \"i\"")
            .no_transaction(),
    );
    plan.push(Statement::new("SELECT 1", ""));
    assert_eq!(
        render_file(&plan, true),
        "-- openmig:generated\n-- openmig:no-transaction\n\n-- Drop index \"i\"\nDROP INDEX CONCURRENTLY \"app\".\"i\";\nSELECT 1;\n"
    );
    plan.statements[0].tx = TxRequirement::Any;
    assert_eq!(
        render_file(&plan, false),
        "-- Drop index \"i\"\nDROP INDEX CONCURRENTLY \"app\".\"i\";\nSELECT 1;\n"
    );
    assert_eq!(render_file(&plan, true).lines().nth(1), Some(""));
}

#[test]
fn render_file_splits_back_into_the_plan() {
    let plan = plan();
    for generated in [false, true] {
        let content = render_file(&plan, generated);
        let statements = split(&content, Dialect::Postgres).unwrap();
        let got: Vec<_> = statements.iter().map(|s| s.sql.as_str()).collect();
        let want: Vec<_> = plan.statements.iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(got, want);
        let comments: Vec<_> = statements.iter().map(|s| s.comments.join("\n")).collect();
        assert_eq!(
            comments,
            [
                "Create table \"app\".\"users\"",
                "Create function \"app\".\"f\"",
                "",
                "multi line"
            ]
        );
        let directives = parse_directives(&content).unwrap();
        assert_eq!(directives.generated, generated);
        assert!(!directives.no_transaction);
    }
}

/// Loads the real migration directories of the dialo repository when it is checked out next to
/// this one: every name is valid and Atlas directives are plain comments.
#[test]
#[ignore = "needs /home/coder/dialo/dialo-db"]
fn dialo_directories_load() {
    let root = Path::new("/home/coder/dialo/dialo-db");
    let mut loaded = 0;
    for sub in ["pg", "ch"] {
        let path = root.join(sub).join("migrations");
        if !path.is_dir() {
            continue;
        }
        let dir = MigrationDir::load(&path).unwrap();
        assert!(!dir.files.is_empty());
        assert!(
            dir.files
                .iter()
                .all(|f| f.directives == Directives::default())
        );
        assert_eq!(dir.verify_sum(), Err(SumError::Missing));
        println!(
            "{sub}: {} files, {} .. {}",
            dir.files.len(),
            dir.files[0].file_name,
            dir.latest_version().unwrap_or_default()
        );
        loaded += 1;
    }
    assert!(loaded > 0, "no dialo migration directories found");
}
