//! Loopback HTTP/1.1 GET. The CLI talks only to a local daemon; it does not
//! need a general-purpose HTTP stack.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// GET `path` from a daemon on `addr`. Returns status and parsed JSON body.
pub async fn get_json(addr: SocketAddr, path: &str) -> Result<(u16, Value)> {
    let (status, body) = get_raw(addr, path).await?;
    if body.trim().is_empty() {
        return Ok((status, Value::Null));
    }
    let json = serde_json::from_str(body.trim())
        .with_context(|| format!("daemon at {addr}{path} returned non-JSON"))?;
    Ok((status, json))
}

async fn get_raw(addr: SocketAddr, path: &str) -> Result<(u16, String)> {
    let connect = TcpStream::connect(addr);
    let mut stream = timeout(REQUEST_TIMEOUT, connect)
        .await
        .with_context(|| format!("connecting to {addr}"))?
        .with_context(|| format!("nothing listening on {addr}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    timeout(REQUEST_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .context("writing request")?
        .context("writing request")?;

    let mut response = Vec::new();
    timeout(REQUEST_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .context("reading response")?
        .context("reading response")?;

    let response = String::from_utf8(response).context("response is not utf-8")?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("HTTP headers did not end")?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .context("HTTP status line")?;
    Ok((status, body.to_string()))
}

/// Fail with a message an agent can act on when the daemon is down.
pub fn unreachable(addr: SocketAddr, source: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "nothing listening on {addr} ({source}); start with `devpulse serve --headless`"
    )
}

pub fn require_ok(status: u16, body: &Value, path: &str) -> Result<Value> {
    if (200..300).contains(&status) {
        return Ok(body.clone());
    }
    if let Some(message) = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        bail!("{path}: {message}");
    }
    bail!("{path}: HTTP {status}");
}
