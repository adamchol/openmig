//! `-- openmig:<directive>` lines in the leading comment block of a migration file.
//!
//! The leading comment block is the run of `--` comment lines (and blank lines) at the top of the
//! file before the first statement. Recognised directives:
//!
//! * `-- openmig:generated` — written by `openmig generate`; `generate --base` may replace the file.
//! * `-- openmig:no-transaction` — run statement by statement outside a transaction.
//! * `-- openmig:lock-timeout <duration>` — lock timeout for this file (`30s`, `2m`).
//! * `-- openmig:statement-timeout <duration>`.
//!
//! Durations are a whole number followed by `ms`, `s`, `m` or `h`. Unknown `openmig:` directives,
//! missing or unexpected arguments and repeated directives are errors. Other comments are ignored.

use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directives {
    pub generated: bool,
    pub no_transaction: bool,
    pub lock_timeout: Option<Duration>,
    pub statement_timeout: Option<Duration>,
}

pub(crate) const GENERATED: &str = "generated";
pub(crate) const NO_TRANSACTION: &str = "no-transaction";
const LOCK_TIMEOUT: &str = "lock-timeout";
const STATEMENT_TIMEOUT: &str = "statement-timeout";

/// Parse directives from the leading comment block of `content`. Errors start with the 1-based
/// line number.
pub fn parse_directives(content: &str) -> Result<Directives, String> {
    let mut directives = Directives::default();
    let mut seen: Vec<&str> = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(comment) = line.strip_prefix("--") else {
            break;
        };
        let Some(directive) = comment.trim().strip_prefix("openmig:") else {
            continue;
        };
        let (name, arg) = match directive.split_once(char::is_whitespace) {
            Some((name, arg)) => (name, arg.trim()),
            None => (directive, ""),
        };
        let line_no = index + 1;
        let fail = |message: String| format!("line {line_no}: {message}");
        if seen.contains(&name) {
            return Err(fail(format!("duplicate directive `openmig:{name}`")));
        }
        match name {
            GENERATED | NO_TRANSACTION => {
                if !arg.is_empty() {
                    return Err(fail(format!(
                        "directive `openmig:{name}` takes no argument, got `{arg}`"
                    )));
                }
                if name == GENERATED {
                    directives.generated = true;
                } else {
                    directives.no_transaction = true;
                }
            }
            LOCK_TIMEOUT | STATEMENT_TIMEOUT => {
                if arg.is_empty() {
                    return Err(fail(format!(
                        "directive `openmig:{name}` needs a duration, e.g. `-- openmig:{name} 30s`"
                    )));
                }
                let duration = parse_duration(arg).map_err(fail)?;
                if name == LOCK_TIMEOUT {
                    directives.lock_timeout = Some(duration);
                } else {
                    directives.statement_timeout = Some(duration);
                }
            }
            _ => {
                return Err(fail(format!(
                    "unknown directive `openmig:{name}` (known: {GENERATED}, {NO_TRANSACTION}, {LOCK_TIMEOUT}, {STATEMENT_TIMEOUT})"
                )));
            }
        }
        seen.push(name);
    }
    Ok(directives)
}

/// `500ms`, `30s`, `5m`, `1h`.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid = || {
        format!(
            "invalid duration `{text}`: expected a whole number followed by ms, s, m or h (e.g. `30s`)"
        )
    };
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let value: u64 = number.parse().map_err(|_| invalid())?;
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.checked_mul(60).ok_or_else(invalid)?),
        "h" => Duration::from_secs(value.checked_mul(3600).ok_or_else(invalid)?),
        _ => return Err(invalid()),
    };
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn all_directives() {
        let content = "\
-- openmig:generated
-- openmig:no-transaction
-- some note
--openmig:lock-timeout 500ms

-- openmig:statement-timeout   5m
CREATE TABLE t (a int);
";
        assert_eq!(
            parse_directives(content).unwrap(),
            Directives {
                generated: true,
                no_transaction: true,
                lock_timeout: Some(Duration::from_millis(500)),
                statement_timeout: Some(Duration::from_secs(300)),
            }
        );
    }

    #[test]
    fn only_the_leading_comment_block_counts() {
        let content = "-- Create table\nCREATE TABLE t (a int);\n-- openmig:no-transaction\n-- openmig:bogus\n";
        assert_eq!(parse_directives(content).unwrap(), Directives::default());
        let content = "/* openmig:no-transaction */\n-- openmig:no-transaction\nSELECT 1;";
        assert_eq!(parse_directives(content).unwrap(), Directives::default());
    }

    #[test]
    fn other_tools_directives_are_plain_comments() {
        let content = "-- othertool:txmode none\n\nCREATE INDEX CONCURRENTLY i ON t (a);\n";
        assert_eq!(parse_directives(content).unwrap(), Directives::default());
    }

    #[test]
    fn crlf() {
        let content = "-- openmig:no-transaction\r\n-- openmig:lock-timeout 2s\r\nSELECT 1;\r\n";
        let d = parse_directives(content).unwrap();
        assert!(d.no_transaction);
        assert_eq!(d.lock_timeout, Some(Duration::from_secs(2)));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        for bad in ["", "s", "10", "1.5s", "10 s", "-1s", "5d", "ms100"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn errors() {
        let cases = [
            (
                "-- openmig:transactionless\n",
                "line 1: unknown directive `openmig:transactionless`",
            ),
            (
                "\n-- openmig:generated yes\n",
                "line 2: directive `openmig:generated` takes no argument",
            ),
            (
                "-- openmig:lock-timeout\n",
                "line 1: directive `openmig:lock-timeout` needs a duration",
            ),
            (
                "-- openmig:statement-timeout soon\n",
                "line 1: invalid duration `soon`",
            ),
            (
                "-- openmig:generated\n-- openmig:generated\n",
                "line 2: duplicate directive",
            ),
            (
                "-- openmig: generated\n",
                "line 1: unknown directive `openmig:`",
            ),
        ];
        for (content, expected) in cases {
            let err = parse_directives(content).unwrap_err();
            assert!(err.starts_with(expected), "{content:?}: {err}");
        }
    }
}
