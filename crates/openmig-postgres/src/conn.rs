//! Connection strings, TLS and error mapping.

use futures_util::StreamExt;
use openmig_core::{Error, ExecError, ExecErrorKind};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio_postgres::config::Host;
use tokio_postgres::error::ErrorPosition;
use tokio_postgres::{AsyncMessage, Client, Config, Connection, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;

/// libpq `sslmode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SslMode {
    Disable,
    /// TLS when the server offers it, without certificate verification.
    Prefer,
    /// TLS without certificate verification.
    Require,
    /// TLS with a certificate signed by a trusted root, for any host name.
    VerifyCa,
    /// TLS with a trusted certificate valid for the host name.
    VerifyFull,
}

/// A parsed connection string.
#[derive(Clone, Debug)]
pub(crate) struct ConnectSpec {
    pub config: Config,
    pub ssl: SslMode,
    /// `sslrootcert`: a PEM file, or `None` for the platform's trust store.
    pub root_cert: Option<PathBuf>,
    /// The connection string with any password masked, for messages.
    pub display: String,
}

const TLS_KEYS: &[&str] = &["sslmode", "sslrootcert"];

impl ConnectSpec {
    /// Parse a `postgres[ql]://` URL or a libpq `key=value` string.
    pub fn parse(conn: &str) -> Result<Self, Error> {
        let display = redact(conn);
        let invalid = |message: String| Error::Connect {
            target: display.clone(),
            message,
        };
        let (rest, tls) = if conn.starts_with("postgres://") || conn.starts_with("postgresql://") {
            split_url_tls(conn)
        } else {
            split_kv_tls(conn).map_err(invalid)?
        };
        let mut ssl = None;
        let mut root_cert = None;
        for (key, value) in tls {
            match key.as_str() {
                "sslmode" => {
                    ssl = Some(match value.as_str() {
                        "disable" | "allow" => SslMode::Disable,
                        "prefer" => SslMode::Prefer,
                        "require" => SslMode::Require,
                        "verify-ca" => SslMode::VerifyCa,
                        "verify-full" => SslMode::VerifyFull,
                        other => return Err(invalid(format!("invalid sslmode `{other}`"))),
                    })
                }
                "sslrootcert" if value != "system" => root_cert = Some(PathBuf::from(value)),
                _ => {}
            }
        }
        let config = Config::from_str(&rest).map_err(|e| invalid(e.to_string()))?;
        let all_unix = !config.get_hosts().is_empty()
            && config
                .get_hosts()
                .iter()
                .all(|h| matches!(h, Host::Unix(_)));
        let ssl = match ssl {
            Some(mode) => mode,
            None if all_unix => SslMode::Disable,
            None => SslMode::Prefer,
        };
        Ok(Self {
            config,
            ssl,
            root_cert,
            display,
        })
    }

    pub fn dbname(&self) -> Option<&str> {
        self.config.get_dbname()
    }

    /// The same server and credentials, another database.
    pub fn with_dbname(&self, dbname: &str) -> Self {
        let mut out = self.clone();
        out.config.dbname(dbname);
        out
    }

    /// Open a connection whose background task forwards server notices to `tracing`.
    pub async fn connect(&self, application_name: &str) -> Result<Client, Error> {
        let mut config = self.config.clone();
        if config.get_application_name().is_none() {
            config.application_name(application_name);
        }
        let fail = |e: tokio_postgres::Error| Error::Connect {
            target: self.display.clone(),
            message: describe(&e),
        };
        if self.ssl == SslMode::Disable {
            config.ssl_mode(tokio_postgres::config::SslMode::Disable);
            let (client, connection) = config.connect(NoTls).await.map_err(fail)?;
            spawn_connection(connection);
            return Ok(client);
        }
        config.ssl_mode(match self.ssl {
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            _ => tokio_postgres::config::SslMode::Require,
        });
        let tls = self.tls().map_err(|message| Error::Connect {
            target: self.display.clone(),
            message,
        })?;
        let (client, connection) = config.connect(tls).await.map_err(fail)?;
        spawn_connection(connection);
        Ok(client)
    }

    fn tls(&self) -> Result<MakeRustlsConnect, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?;
        let config = match self.ssl {
            SslMode::Disable | SslMode::Prefer | SslMode::Require => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
                .with_no_client_auth(),
            SslMode::VerifyCa => {
                let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(self.roots()?),
                    provider,
                )
                .build()
                .map_err(|e| e.to_string())?;
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(IgnoreHostName(inner)))
                    .with_no_client_auth()
            }
            SslMode::VerifyFull => builder
                .with_root_certificates(self.roots()?)
                .with_no_client_auth(),
        };
        Ok(MakeRustlsConnect::new(config))
    }

    fn roots(&self) -> Result<RootCertStore, String> {
        let mut roots = RootCertStore::empty();
        match &self.root_cert {
            Some(path) => {
                let certs = CertificateDer::pem_file_iter(path)
                    .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                    .map_err(|e| format!("cannot read sslrootcert {}: {e}", path.display()))?;
                roots.add_parsable_certificates(certs);
            }
            None => {
                let loaded = rustls_native_certs::load_native_certs();
                for e in &loaded.errors {
                    tracing::debug!("loading native certificates: {e}");
                }
                roots.add_parsable_certificates(loaded.certs);
            }
        }
        if roots.is_empty() {
            return Err("no trusted root certificates found (set sslrootcert)".into());
        }
        Ok(roots)
    }
}

fn spawn_connection<S, T>(mut connection: Connection<S, T>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(AsyncMessage::Notice(n)) => match n.severity() {
                    // Shutdown notices arrive when a scratch server stops under an open session.
                    "WARNING" if !n.code().code().starts_with("57P") => {
                        tracing::warn!(target: "openmig::postgres", "{}", n.message())
                    }
                    severity => {
                        tracing::debug!(target: "openmig::postgres", severity, "{}", n.message())
                    }
                },
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(target: "openmig::postgres", "connection closed: {e}");
                    break;
                }
            }
        }
    });
}

/// Split TLS parameters (which tokio-postgres does not fully support) from a URL's query.
fn split_url_tls(url: &str) -> (String, Vec<(String, String)>) {
    let Some((base, query)) = url.split_once('?') else {
        return (url.to_owned(), Vec::new());
    };
    let mut kept = Vec::new();
    let mut tls = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let decoded = url::form_urlencoded::parse(pair.as_bytes()).next();
        match decoded {
            Some((k, v)) if TLS_KEYS.contains(&k.as_ref()) => {
                tls.push((k.into_owned(), v.into_owned()))
            }
            _ => kept.push(pair),
        }
    }
    let rest = if kept.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", kept.join("&"))
    };
    (rest, tls)
}

/// Split TLS parameters from a libpq `key=value` string and re-serialize the rest.
fn split_kv_tls(conn: &str) -> Result<(String, Vec<(String, String)>), String> {
    let mut kept = Vec::new();
    let mut tls = Vec::new();
    for (key, value) in parse_kv(conn)? {
        if TLS_KEYS.contains(&key.as_str()) {
            tls.push((key, value));
        } else {
            let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
            kept.push(format!("{key}='{escaped}'"));
        }
    }
    Ok((kept.join(" "), tls))
}

fn parse_kv(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some('=') {
            return Err(format!("missing `=` after `{key}` in connection string"));
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next() {
                    Some('\\') => value.extend(chars.next()),
                    Some('\'') => break,
                    Some(c) => value.push(c),
                    None => return Err("unterminated quoted value in connection string".into()),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\' {
                    value.extend(chars.next());
                } else {
                    value.push(c);
                }
            }
        }
        out.push((key, value));
    }
}

/// Mask the password in a connection string.
pub(crate) fn redact(conn: &str) -> String {
    if let Some(scheme_end) = conn.find("://") {
        let after = &conn[scheme_end + 3..];
        let authority_end = after.find(['/', '?']).unwrap_or(after.len());
        let authority = &after[..authority_end];
        let mut out = conn.to_owned();
        if let Some(at) = authority.rfind('@')
            && let Some(colon) = authority[..at].find(':')
        {
            let start = scheme_end + 3 + colon + 1;
            let end = scheme_end + 3 + at;
            out.replace_range(start..end, "***");
        }
        if let Some((base, query)) = out.clone().split_once('?') {
            let pairs: Vec<String> = query
                .split('&')
                .map(|p| match p.split_once('=') {
                    Some(("password", _)) => "password=***".to_owned(),
                    _ => p.to_owned(),
                })
                .collect();
            out = format!("{base}?{}", pairs.join("&"));
        }
        return out;
    }
    match parse_kv(conn) {
        Ok(pairs) => pairs
            .into_iter()
            .map(|(k, v)| {
                if k == "password" {
                    format!("{k}=***")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        Err(_) => "<invalid connection string>".into(),
    }
}

/// `require` and `prefer`: encryption without authentication of the server, like libpq.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `verify-ca`: a trusted chain is required, the host name is not checked.
#[derive(Debug)]
struct IgnoreHostName(Arc<rustls::client::WebPkiServerVerifier>);

impl ServerCertVerifier for IgnoreHostName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// A one-line description of a client error, including the server's message when there is one.
pub(crate) fn describe(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!("{} ({})", db.message(), db.code().code()),
        None => {
            let mut message = e.to_string();
            let mut source = std::error::Error::source(e);
            while let Some(s) = source {
                message = format!("{message}: {s}");
                source = s.source();
            }
            message
        }
    }
}

/// Map a client error to an [`ExecError`] with SQLSTATE, details and a retry classification.
pub(crate) fn exec_error(e: &tokio_postgres::Error) -> ExecError {
    let Some(db) = e.as_db_error() else {
        let connection =
            e.is_closed() || std::error::Error::source(e).is_some_and(|s| s.is::<std::io::Error>());
        return ExecError {
            kind: if connection {
                ExecErrorKind::Connection
            } else {
                ExecErrorKind::Other
            },
            message: describe(e),
            ..ExecError::default()
        };
    };
    let code = db.code().code();
    let kind = match code {
        "55P03" => ExecErrorKind::LockTimeout,
        "42P01" | "42883" | "42704" | "3F000" => ExecErrorKind::UndefinedObject,
        "57P01" | "57P02" | "57P03" => ExecErrorKind::Connection,
        c if c.starts_with("08") => ExecErrorKind::Connection,
        _ => ExecErrorKind::Other,
    };
    ExecError {
        kind,
        message: db.message().to_owned(),
        code: Some(code.to_owned()),
        detail: db.detail().map(str::to_owned),
        hint: db.hint().map(str::to_owned),
        position: match db.position() {
            Some(ErrorPosition::Original(p)) => usize::try_from(*p).ok(),
            _ => None,
        },
        statement: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_socket_url() {
        let spec =
            ConnectSpec::parse("postgresql:///app?host=/tmp/pg&port=5491&user=openmig").unwrap();
        assert_eq!(spec.ssl, SslMode::Disable);
        assert_eq!(spec.dbname(), Some("app"));
        assert_eq!(spec.config.get_user(), Some("openmig"));
        assert_eq!(spec.config.get_ports(), &[5491]);
        assert!(
            matches!(&spec.config.get_hosts()[0], Host::Unix(p) if p.to_str() == Some("/tmp/pg"))
        );
    }

    #[test]
    fn tls_modes_are_extracted() {
        let spec = ConnectSpec::parse(
            "postgres://u:secret@db.example.com:6432/app?sslmode=verify-full&sslrootcert=%2Fca.pem&application_name=x",
        )
        .unwrap();
        assert_eq!(spec.ssl, SslMode::VerifyFull);
        assert_eq!(spec.root_cert, Some(PathBuf::from("/ca.pem")));
        assert_eq!(spec.config.get_password(), Some(&b"secret"[..]));
        assert_eq!(spec.config.get_application_name(), Some("x"));
        assert_eq!(
            spec.display,
            "postgres://u:***@db.example.com:6432/app?sslmode=verify-full&sslrootcert=%2Fca.pem&application_name=x"
        );
        let spec = ConnectSpec::parse("postgres://db/app").unwrap();
        assert_eq!(spec.ssl, SslMode::Prefer);
        assert!(ConnectSpec::parse("postgres://db/app?sslmode=bogus").is_err());
    }

    #[test]
    fn key_value_strings() {
        let spec = ConnectSpec::parse(
            "host=db port=5433 user=app password='p w\\'d' dbname=x sslmode=require",
        )
        .unwrap();
        assert_eq!(spec.ssl, SslMode::Require);
        assert_eq!(spec.config.get_password(), Some(&b"p w'd"[..]));
        assert_eq!(spec.dbname(), Some("x"));
        assert_eq!(
            spec.display,
            "host=db port=5433 user=app password=*** dbname=x sslmode=require"
        );
        assert_eq!(spec.with_dbname("y").dbname(), Some("y"));
    }
}
