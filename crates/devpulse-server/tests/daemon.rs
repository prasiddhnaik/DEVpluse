//! Whole-daemon tests (task T3.1): the real snapshot loop, the real socket.
//!
//! These are the only server tests that observe the machine they run on, so
//! they assert on things that are true of any machine — that a tick happened
//! at all — rather than on what happens to be running.

use std::net::SocketAddr;
use std::time::Duration;

use devpulse_server::daemon::{Daemon, DaemonConfig};
use devpulse_server::snapshot::SnapshotConfig;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn config(bind: &str) -> DaemonConfig {
    DaemonConfig {
        bind: bind.parse().expect("addr"),
        snapshot: SnapshotConfig {
            tick_interval: Duration::from_millis(100),
            ..SnapshotConfig::default()
        },
        // Probing Docker would make the test depend on whether the machine
        // has a daemon; T6.1 covers that separately.
        probe_docker: false,
        // Never touch the developer's real database from a test.
        database: None,
        // The watcher is exercised by its own tests; a daemon test should not
        // depend on what the machine's filesystem does while it runs.
        watch_files: false,
        ..DaemonConfig::default()
    }
}

/// Minimal HTTP/1.1 GET. The daemon's only client that matters is a browser;
/// a test does not need more than this.
async fn http_get(addr: SocketAddr, path: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: */*\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    let response = String::from_utf8_lossy(&response).into_owned();

    let (head, body) = response.split_once("\r\n\r\n").expect("headers end");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");

    let json = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn a_non_loopback_bind_is_refused() {
    // `AGENTS.md` rule 6: the daemon must never be reachable from the network,
    // and refusing is enforced here rather than trusted from configuration.
    let error = match Daemon::bind(config("0.0.0.0:0")).await {
        Err(error) => error,
        Ok(_) => panic!("wildcard bind must be refused"),
    };
    assert!(
        error.to_string().contains("loopback"),
        "the error must say why: {error}"
    );
}

#[tokio::test]
async fn the_daemon_serves_status_and_keeps_ticking() {
    let daemon = Daemon::bind(config("127.0.0.1:0"))
        .await
        .expect("binds loopback");
    let addr = daemon.local_addr().expect("addr");
    assert!(addr.ip().is_loopback());

    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        daemon
            .serve_until(async {
                let _ = stopped.await;
            })
            .await
    });

    // Two tick intervals plus slack: enough for the first collection to land.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let (status, body) = http_get(addr, "/api/v1/status").await;
    assert_eq!(status, 200);
    assert_eq!(body["platform"]["os"], std::env::consts::OS);
    assert!(
        body["collectors"]["process"]["last_run"].is_string(),
        "the snapshot loop must have run at least once: {body}"
    );
    assert!(body["uptime_ms"].as_u64().expect("uptime") > 0);

    // Any developer machine runs *something*, including this test binary.
    assert!(
        body["counts"]["services"].as_u64().expect("services") > 0,
        "expected at least one service to be discovered: {body}"
    );

    let (status, projects) = http_get(addr, "/api/v1/projects").await;
    assert_eq!(status, 200);
    assert!(projects.is_array());

    let _ = shutdown.send(());
    served
        .await
        .expect("server task joins")
        .expect("server exits cleanly");
}
