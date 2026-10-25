//! The ClickHouse HTTP interface: connection URLs, requests and exception parsing.

use openmig_core::{Error, ExecError, ExecErrorKind};
use percent_encoding::percent_decode_str;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Settings for every read: plain JSON numbers and errors reported with the HTTP status.
const READ_SETTINGS: &[(&str, &str)] = &[
    ("wait_end_of_query", "1"),
    ("output_format_json_quote_64bit_integers", "0"),
    ("output_format_json_quote_64bit_floats", "0"),
    ("output_format_json_quote_decimals", "0"),
    ("output_format_json_named_tuples_as_objects", "1"),
];

/// Error codes of statements that reference an object that does not exist (yet).
const UNDEFINED_OBJECT_CODES: &[&str] = &[
    "UNKNOWN_TABLE",
    "UNKNOWN_DATABASE",
    "UNKNOWN_FUNCTION",
    "UNKNOWN_IDENTIFIER",
    "UNKNOWN_ROLE",
    "UNKNOWN_USER",
    "UNKNOWN_ROW_POLICY",
    "UNKNOWN_QUOTA",
    "ACCESS_ENTITY_NOT_FOUND",
    "NAMED_COLLECTION_DOESNT_EXIST",
    "CANNOT_GET_CREATE_TABLE_QUERY",
];

/// A parsed `http(s)://user:password@host:port[/database][?setting=value]` URL and an HTTP
/// client for it.
#[derive(Clone, Debug)]
pub(crate) struct HttpClient {
    client: reqwest::Client,
    endpoint: Url,
    user: String,
    password: String,
    database: Option<String>,
    /// Query parameters of the URL, passed through as settings.
    params: Vec<(String, String)>,
    /// The URL without its password, for messages.
    target: String,
}

impl HttpClient {
    pub(crate) fn new(url: &str) -> Result<Self, Error> {
        let parsed = Url::parse(url)
            .map_err(|e| Error::Config(format!("invalid ClickHouse URL `{}`: {e}", redact(url))))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(Error::Config(format!(
                "invalid ClickHouse URL `{}`: expected http(s)://user:password@host:port[/database]",
                redact(url)
            )));
        }
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        let user = match parsed.username() {
            "" => "default".to_owned(),
            u => decode(u),
        };
        let password = parsed.password().map(decode).unwrap_or_default();
        let database = Some(decode(parsed.path().trim_matches('/'))).filter(|d| !d.is_empty());
        let params = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut endpoint = parsed.clone();
        let _ = endpoint.set_username("");
        let _ = endpoint.set_password(None);
        endpoint.set_path("/");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let mut target = parsed;
        let _ = target.set_password(None);
        target.set_query(None);
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_backend_preconfigured(tls_config())
            .build()
            .map_err(|e| Error::Connect {
                target: target.to_string(),
                message: e.to_string(),
            })?;
        Ok(Self {
            client,
            endpoint,
            user,
            password,
            database,
            params,
            target: target.to_string(),
        })
    }

    /// The URL without its password.
    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    /// Run one statement and return the response body.
    pub(crate) async fn query(
        &self,
        sql: &str,
        settings: &[(&str, &str)],
    ) -> Result<String, ExecError> {
        let mut url = self.endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            for (k, v) in &self.params {
                query.append_pair(k, v);
            }
            if let Some(database) = &self.database {
                query.append_pair("database", database);
            }
            for (k, v) in settings {
                query.append_pair(k, v);
            }
        }
        let mut request = self
            .client
            .post(url)
            .header("X-ClickHouse-User", &self.user)
            .body(sql.to_owned());
        if !self.password.is_empty() {
            request = request.header("X-ClickHouse-Key", &self.password);
        }
        let response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        let code = response
            .headers()
            .get("X-ClickHouse-Exception-Code")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.map_err(transport_error)?;
        if status.is_success() && code.is_none() {
            Ok(body)
        } else {
            Err(parse_exception(code.as_deref(), &body))
        }
    }

    /// Run a `SELECT` and decode its rows (`FORMAT JSONEachRow` is appended).
    pub(crate) async fn rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        settings: &[(&str, &str)],
    ) -> Result<Vec<T>, ExecError> {
        let mut all: Vec<(&str, &str)> = READ_SETTINGS.to_vec();
        all.extend_from_slice(settings);
        let body = self
            .query(&format!("{sql}\nFORMAT JSONEachRow"), &all)
            .await?;
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|e| {
                    ExecError::new(format!("cannot decode a row of `{sql}`: {e}: {line}"))
                })
            })
            .collect()
    }
}

/// TLS settings for `https` URLs: the platform's trusted roots, loaded leniently. Without any
/// (minimal container images, build sandboxes) the client still builds and an `https`
/// connection fails at the handshake with an unknown-issuer error.
fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for error in &loaded.errors {
        tracing::debug!("loading native certificates: {error}");
    }
    roots.add_parsable_certificates(loaded.certs);
    if roots.is_empty() {
        tracing::debug!("no trusted root certificates found; https connections will fail");
    }
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

fn transport_error(error: reqwest::Error) -> ExecError {
    let mut source: Option<&dyn std::error::Error> = Some(&error);
    let mut message = String::new();
    while let Some(e) = source {
        if !message.is_empty() {
            message.push_str(": ");
        }
        message.push_str(&e.to_string());
        source = e.source();
    }
    ExecError {
        kind: ExecErrorKind::Connection,
        message,
        ..ExecError::default()
    }
}

/// `scheme://user:***@host/...` for messages.
pub(crate) fn redact(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("***"));
            }
            parsed.to_string()
        }
        Err(_) => url.to_owned(),
    }
}

/// Turn a ClickHouse exception into an [`ExecError`].
///
/// The body looks like `Code: 60. DB::Exception: Table app.t does not exist. (UNKNOWN_TABLE)
/// (version 26.5.1.882)`; the numeric code is also sent in `X-ClickHouse-Exception-Code`.
pub(crate) fn parse_exception(header_code: Option<&str>, body: &str) -> ExecError {
    let text = body.trim();
    let mut rest = text;
    let mut numeric = header_code.map(str::to_owned);
    if let Some(after) = rest.strip_prefix("Code: ") {
        let end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if end > 0 {
            numeric.get_or_insert_with(|| after[..end].to_owned());
            rest = after[end..].trim_start_matches(['.', ',']).trim_start();
        }
    }
    rest = rest
        .strip_prefix("e.displayText() = ")
        .unwrap_or(rest)
        .trim_start();
    rest = rest.strip_prefix("DB::Exception: ").unwrap_or(rest);
    if let Some(version) = rest.rfind(" (version ") {
        rest = rest[..version].trim_end();
    }
    let mut name = None;
    if let Some(stripped) = rest.strip_suffix(')')
        && let Some(open) = stripped.rfind('(')
    {
        let candidate = &stripped[open + 1..];
        if !candidate.is_empty()
            && candidate
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            name = Some(candidate.to_owned());
            rest = stripped[..open].trim_end();
        }
    }
    let message = if rest.is_empty() { text } else { rest };
    let code = name.or(numeric);
    let kind = match code.as_deref() {
        Some(c) if UNDEFINED_OBJECT_CODES.contains(&c) => ExecErrorKind::UndefinedObject,
        Some("BAD_ARGUMENTS")
            if message.contains("Dictionary") && message.contains("not found") =>
        {
            ExecErrorKind::UndefinedObject
        }
        _ => ExecErrorKind::Other,
    };
    ExecError {
        kind,
        message: message.to_owned(),
        code,
        ..ExecError::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exceptions() {
        let e = parse_exception(
            Some("81"),
            "Code: 81. DB::Exception: Database nope does not exist. (UNKNOWN_DATABASE) (version 26.5.1.882)\n",
        );
        assert_eq!(e.code.as_deref(), Some("UNKNOWN_DATABASE"));
        assert_eq!(e.message, "Database nope does not exist.");
        assert_eq!(e.kind, ExecErrorKind::UndefinedObject);

        let e = parse_exception(
            Some("62"),
            "Code: 62. DB::Exception: Syntax error: failed at position 1 ('x'): x. Expected one of: (SELECT, WITH). (SYNTAX_ERROR) (version 26.5.1.882)",
        );
        assert_eq!(e.code.as_deref(), Some("SYNTAX_ERROR"));
        assert!(
            e.message
                .starts_with("Syntax error: failed at position 1 ('x'): x. Expected one of")
        );
        assert_eq!(e.kind, ExecErrorKind::Other);

        let e = parse_exception(
            None,
            "Code: 36. DB::Exception: Dictionary (`app.dict`) not found: In scope SELECT dictGet('app.dict', 'a', 1). (BAD_ARGUMENTS) (version 26.5.1.882)",
        );
        assert_eq!(e.kind, ExecErrorKind::UndefinedObject);

        let e = parse_exception(
            Some("511"),
            "Code: 511. DB::Exception: There is no role `r` in `user directories`. (UNKNOWN_ROLE) (version 26.5.1.882)",
        );
        assert_eq!(e.code.as_deref(), Some("UNKNOWN_ROLE"));
        assert_eq!(e.kind, ExecErrorKind::UndefinedObject);

        let e = parse_exception(
            Some("60"),
            "Code: 60, e.displayText() = DB::Exception: Table default.x doesn't exist (version 21.8.1)",
        );
        assert_eq!(e.code.as_deref(), Some("60"));
        assert_eq!(e.message, "Table default.x doesn't exist");

        let e = parse_exception(Some("999"), "garbage");
        assert_eq!(e.code.as_deref(), Some("999"));
        assert_eq!(e.message, "garbage");
    }

    #[test]
    fn urls() {
        let client =
            HttpClient::new("https://app%40x:p%2Fw@ch.example:8443/analytics?max_threads=2")
                .unwrap();
        assert_eq!(client.user, "app@x");
        assert_eq!(client.password, "p/w");
        assert_eq!(client.database.as_deref(), Some("analytics"));
        assert_eq!(client.params, vec![("max_threads".into(), "2".into())]);
        assert_eq!(client.endpoint.as_str(), "https://ch.example:8443/");
        assert_eq!(client.target(), "https://app%40x@ch.example:8443/analytics");

        let scratch = HttpClient::new("http://default@127.0.0.1:8123/").unwrap();
        assert_eq!(scratch.user, "default");
        assert_eq!(scratch.password, "");
        assert_eq!(scratch.database, None);

        assert!(matches!(
            HttpClient::new("clickhouse://localhost:9000"),
            Err(Error::Config(_))
        ));
        assert!(matches!(
            HttpClient::new("not a url"),
            Err(Error::Config(_))
        ));
        assert_eq!(redact("http://u:secret@h:1/db"), "http://u:***@h:1/db");
    }
}
