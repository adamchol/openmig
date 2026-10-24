//! Parsing of the few catalog values that are only available as text.

use std::collections::BTreeMap;

/// `reloptions` (`{fillfactor=70,security_barrier=true}`) as a map.
pub(super) fn options(raw: Vec<String>) -> BTreeMap<String, String> {
    raw.into_iter()
        .map(|o| match o.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (o, String::new()),
        })
        .collect()
}

/// Trigger arguments from `tgargs`: `tgnargs` NUL-terminated strings.
pub(super) fn trigger_args(raw: &[u8], count: usize) -> Vec<String> {
    raw.split(|b| *b == 0)
        .take(count)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The `WHEN` condition of a trigger definition printed by `pg_get_triggerdef`, without the
/// parentheses around it.
pub(super) fn trigger_when(def: &str) -> Option<String> {
    let start = ["FOR EACH ROW WHEN (", "FOR EACH STATEMENT WHEN ("]
        .iter()
        .filter_map(|p| find_unquoted(def, p).map(|i| i + p.len()))
        .min()?;
    let end = closing_paren(def, start)?;
    Some(def[start..end].to_owned())
}

/// Byte offset of the first occurrence of `pattern` outside quoted identifiers and string
/// literals.
fn find_unquoted(s: &str, pattern: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if s[i..].starts_with(pattern) => return Some(i),
            None => {}
        }
        i += 1;
    }
    None
}

/// Offset of the `)` closing a parenthesis opened just before `start`, skipping quoted text.
/// A doubled quote inside a quoted run toggles twice, which keeps the scan correct.
fn closing_paren(s: &str, start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut quote: Option<u8> = None;
    for (i, b) in s.bytes().enumerate().skip(start) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_condition() {
        let def = "CREATE TRIGGER tr BEFORE UPDATE OF e, s ON app.t FOR EACH ROW WHEN \
                   (old.* IS DISTINCT FROM new.* AND new.v::text <> ') EXECUTE FUNCTION x'::text) \
                   EXECUTE FUNCTION app.trg('a', 'b''c', 'x y')";
        assert_eq!(
            trigger_when(def).as_deref(),
            Some("old.* IS DISTINCT FROM new.* AND new.v::text <> ') EXECUTE FUNCTION x'::text")
        );
        let def = "CREATE TRIGGER \"FOR EACH ROW WHEN (\" AFTER INSERT ON t FOR EACH STATEMENT \
                   WHEN (f((1))) EXECUTE FUNCTION g()";
        assert_eq!(trigger_when(def).as_deref(), Some("f((1))"));
        assert_eq!(
            trigger_when("CREATE TRIGGER t AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION g()"),
            None
        );
    }

    #[test]
    fn args_and_options() {
        assert_eq!(trigger_args(b"a\0b'c\0\0", 3), vec!["a", "b'c", ""]);
        assert_eq!(trigger_args(b"", 0), Vec::<String>::new());
        let o = options(vec!["fillfactor=70".into(), "check_option=local".into()]);
        assert_eq!(o["fillfactor"], "70");
        assert_eq!(o["check_option"], "local");
    }
}
