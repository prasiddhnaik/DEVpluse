//! Agent CLI against a live daemon. These tests start a real snapshot loop so
//! `devpulse now` is proven against the same API a coding agent would call.

use std::time::Duration;

use devpulse_cli::agent;
use devpulse_server::daemon::{Daemon, DaemonConfig};
use devpulse_server::snapshot::SnapshotConfig;

fn test_config() -> DaemonConfig {
    DaemonConfig {
        bind: "127.0.0.1:0".parse().expect("addr"),
        snapshot: SnapshotConfig {
            tick_interval: Duration::from_millis(50),
            ..SnapshotConfig::default()
        },
        probe_docker: false,
        database: None,
        watch_files: false,
        ..DaemonConfig::default()
    }
}

#[tokio::test]
async fn now_returns_a_compact_snapshot_from_a_live_daemon() {
    let daemon = Daemon::bind(test_config()).await.expect("bind");
    let addr = daemon.local_addr().expect("addr");
    let state = daemon.state();
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    });

    agent::wait_for_first_tick(&state, Duration::from_secs(5))
        .await
        .expect("first tick");

    let now = agent::fetch_now(addr.port(), None, 5).await.expect("now");
    assert_eq!(now["ok"], true);
    assert!(now.get("projects").and_then(|v| v.as_array()).is_some());
    assert!(now.get("warnings").and_then(|v| v.as_array()).is_some());
    assert!(now.get("events").and_then(|v| v.as_array()).is_some());
    assert!(now.get("platform").is_none());
    assert!(now.get("resource_history").is_none());

    let status = agent::fetch_path(addr.port(), "/api/v1/status")
        .await
        .expect("status");
    assert!(status.get("version").is_some());

    let ready = agent::ready_line(addr);
    assert_eq!(ready["ok"], true);
    assert!(
        ready["http"]
            .as_str()
            .expect("http")
            .starts_with("http://127.0.0.1:")
    );
}

#[tokio::test]
async fn fetch_fails_when_nothing_is_listening() {
    let error = agent::fetch_now(59999, None, 5)
        .await
        .expect_err("port 59999 should be closed");
    let message = error.to_string();
    assert!(
        message.contains("59999") && message.contains("serve --headless"),
        "agent-facing hint missing: {message}"
    );
}
