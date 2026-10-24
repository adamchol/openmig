use std::time::Duration;

/// Parse durations like `500ms`, `30s`, `5m`, `1h`, `2m30s`. A bare number means seconds.
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return Err(format!("invalid duration `{input}`"));
        }
        let n: u64 = rest[..digits]
            .parse()
            .map_err(|_| format!("invalid duration `{input}`"))?;
        rest = &rest[digits..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        total += match unit {
            "ms" => Duration::from_millis(n),
            "s" => Duration::from_secs(n),
            "m" => Duration::from_secs(n * 60),
            "h" => Duration::from_secs(n * 3600),
            _ => {
                return Err(format!(
                    "invalid duration unit `{unit}` in `{input}` (use ms, s, m, h)"
                ));
            }
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m30s").unwrap(), Duration::from_secs(150));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("m").is_err());
    }
}
