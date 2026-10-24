//! Minimal HTTP/1.0 client for the local ClickHouse server (readiness probes and admin queries).
//!
//! HTTP/1.0 makes the server close the connection after the response and never use chunked
//! transfer encoding, so the response is simply everything up to EOF.

use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: String,
}

pub(crate) async fn request(
    port: u16,
    method: &str,
    target: &str,
    body: &str,
) -> io::Result<Response> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let head = format!(
        "{method} {target} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> io::Result<Response> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP response");
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(invalid)?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| invalid())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(invalid)?;
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&raw[split + 4..]).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body() {
        let response = parse_response(
            b"HTTP/1.0 404 Not Found\r\nContent-Type: text/plain\r\n\r\nCode: 60.\n",
        )
        .unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.body, "Code: 60.\n");
        assert!(parse_response(b"garbage").is_err());
    }
}
