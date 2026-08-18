//! Agent-facing queries against a running daemon.
//!
//! These commands exist so a coding agent can read the local runtime without
//! the dashboard and without pulling 300-sample sparklines into its context.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::http::{get_json, require_ok, unreachable};
use devpulse_server::state::AppState;

/// Default events included in `devpulse now`. Small enough for a prompt.
pub const NOW_EVENT_LIMIT: usize = 20;

pub fn daemon_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// Compact machine snapshot: projects, warnings, recent events. No resource
/// history, no platform notes, no evidence arrays.
pub fn compact_now(status: &Value, projects: &Value, warnings: &Value, events: &Value) -> Value {
    json!({
        "ok": true,
        "version": status.get("version").cloned().unwrap_or(Value::Null),
        "uptime_ms": status.get("uptime_ms").cloned().unwrap_or(Value::Null),
        "counts": status.get("counts").cloned().unwrap_or(Value::Null),
        "docker": status.pointer("/docker/available").cloned().unwrap_or(Value::Bool(false)),
        "collectors": {
            "process_ms": status.pointer("/collectors/process/last_duration_ms").cloned().unwrap_or(Value::Null),
            "socket_ms": status.pointer("/collectors/socket/last_duration_ms").cloned().unwrap_or(Value::Null),
        },
        "projects": compact_projects(projects),
        "warnings": compact_warnings(warnings),
        "events": compact_events(events),
    })
}

fn compact_projects(projects: &Value) -> Value {
    let Some(rows) = projects.as_array() else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        rows.iter()
            .map(|p| {
                json!({
                    "id": p.get("id").cloned().unwrap_or(Value::Null),
                    "name": p.get("name").cloned().unwrap_or(Value::Null),
                    "root": p.get("root").cloned().unwrap_or(Value::Null),
                    "health": p.get("health").cloned().unwrap_or(Value::Null),
                    "running": p.get("running_service_count").cloned().unwrap_or(Value::Null),
                    "services": p.get("service_count").cloned().unwrap_or(Value::Null),
                    "cpu_percent": p.get("cpu_percent").cloned().unwrap_or(Value::Null),
                    "memory_bytes": p.get("memory_bytes").cloned().unwrap_or(Value::Null),
                    "warning": p.pointer("/recent_warning/message").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

fn compact_warnings(warnings: &Value) -> Value {
    let Some(rows) = warnings.as_array() else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        rows.iter()
            .map(|w| {
                json!({
                    "id": w.get("id").cloned().unwrap_or(Value::Null),
                    "rule": w.get("rule").cloned().unwrap_or(Value::Null),
                    "severity": w.get("severity").cloned().unwrap_or(Value::Null),
                    "message": w.get("message").cloned().unwrap_or(Value::Null),
                    "project_id": w.get("project_id").cloned().unwrap_or(Value::Null),
                    "service_id": w.get("service_id").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

fn compact_events(events: &Value) -> Value {
    let Some(rows) = events.as_array() else {
        return Value::Array(Vec::new());
    };
    Value::Array(
        rows.iter()
            .map(|e| {
                json!({
                    "id": e.get("id").cloned().unwrap_or(Value::Null),
                    "at": e.get("at").cloned().unwrap_or(Value::Null),
                    "project_id": e.get("project_id").cloned().unwrap_or(Value::Null),
                    "kind": e.get("kind").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
    )
}

/// Keep projects whose root contains `cwd` (or vice versa). Used by `--here`.
pub fn filter_projects_here(projects: Value, cwd: &Path) -> Value {
    let Some(rows) = projects.as_array() else {
        return projects;
    };
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let kept: Vec<Value> = rows
        .iter()
        .filter(|p| {
            let Some(root) = p.get("root").and_then(Value::as_str) else {
                return false;
            };
            let root = Path::new(root);
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            cwd.starts_with(&root) || root.starts_with(&cwd)
        })
        .cloned()
        .collect();
    Value::Array(kept)
}

/// Drop fields that burn tokens and do not help an agent act.
pub fn strip_heavy(mut value: Value) -> Value {
    strip_keys(
        &mut value,
        &[
            "resource_history",
            "platform",
            "evidence",
            "fingerprint",
            "degraded_fields",
        ],
    );
    value
}

fn strip_keys(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in keys {
                map.remove(*key);
            }
            for child in map.values_mut() {
                strip_keys(child, keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_keys(item, keys);
            }
        }
        _ => {}
    }
}

pub async fn fetch_now(port: u16, here: Option<&Path>, event_limit: usize) -> Result<Value> {
    let addr = daemon_addr(port);
    let status = fetch(addr, "/api/v1/status").await?;
    let mut projects = fetch(addr, "/api/v1/projects").await?;
    if let Some(cwd) = here {
        projects = filter_projects_here(projects, cwd);
    }
    let warnings = fetch(addr, "/api/v1/warnings").await?;
    let events_path = format!("/api/v1/events?limit={}", event_limit.clamp(1, 1000));
    let events = fetch(addr, &events_path).await?;
    Ok(compact_now(&status, &projects, &warnings, &events))
}

pub async fn fetch_path(port: u16, path: &str) -> Result<Value> {
    fetch(daemon_addr(port), path).await
}

async fn fetch(addr: SocketAddr, path: &str) -> Result<Value> {
    let (status, body) = get_json(addr, path)
        .await
        .map_err(|err| unreachable(addr, err))?;
    require_ok(status, &body, path)
}

/// Block until the snapshot loop has produced one tick, so a headless agent
/// does not query an empty world.
pub async fn wait_for_first_tick(state: &AppState, budget: Duration) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        let status = state.status().await;
        if status.collectors.process.last_run.is_some() {
            return Ok(());
        }
        if started.elapsed() >= budget {
            bail!(
                "first snapshot did not complete within {}s",
                budget.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn ready_line(addr: SocketAddr) -> Value {
    json!({
        "ok": true,
        "http": format!("http://{addr}"),
        "ws": format!("ws://{addr}/ws/v1"),
        "now": format!("devpulse now --port {}", addr.port()),
    })
}

pub fn print_json(pretty: bool, value: &Value) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

pub fn print_unreachable(addr: SocketAddr, err: impl std::fmt::Display) {
    let value = json!({
        "ok": false,
        "error": format!("{err}"),
        "hint": format!("devpulse serve --headless --port {}", addr.port()),
    });
    let _ = print_json(false, &value);
}

/// Frame types an agent typically cares about. Snapshot is omitted: it is the
/// whole world and wastes the context window.
pub fn default_watch_types() -> Vec<String> {
    vec![
        "events".into(),
        "warnings_changed".into(),
        "services_changed".into(),
        "topology_changed".into(),
    ]
}

pub fn parse_watch_types(raw: &str) -> Vec<String> {
    let types: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if types.is_empty() {
        default_watch_types()
    } else {
        types
    }
}

pub fn watch_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}/ws/v1")
}

pub fn frame_type(frame: &Value) -> Option<&str> {
    frame.get("type").and_then(Value::as_str)
}

/// Stream daemon frames as NDJSON until the socket closes or the process is
/// interrupted. Snapshot frames are skipped unless `include_snapshot` is set.
pub async fn watch(port: u16, types: &[String], include_snapshot: bool) -> Result<()> {
    let addr = daemon_addr(port);
    let url = watch_url(port);
    let (mut socket, _) = connect_async(url.as_str())
        .await
        .map_err(|err| unreachable(addr, err))?;

    while let Some(message) = socket.next().await {
        match message {
            Ok(Message::Text(text)) => {
                let frame: Value = serde_json::from_str(&text).context("websocket frame")?;
                let ty = frame_type(&frame).unwrap_or("");
                if ty == "snapshot" && !include_snapshot {
                    continue;
                }
                let wanted =
                    types.iter().any(|name| name == ty) || (include_snapshot && ty == "snapshot");
                if wanted {
                    println!("{}", serde_json::to_string(&frame)?);
                }
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_now_drops_platform_notes_and_warning_bodies() {
        let status = json!({
            "version": "0.1.0",
            "uptime_ms": 12,
            "counts": {"projects": 1, "services": 2, "connections": 0, "events": 3},
            "docker": {"available": false, "reason": "no socket"},
            "collectors": {
                "process": {"last_duration_ms": 8, "last_run": "2026-08-18T00:00:00Z"},
                "socket": {"last_duration_ms": 2}
            },
            "platform": {"notes": ["long"]}
        });
        let projects = json!([{
            "id": "prj_1",
            "name": "app",
            "root": "/tmp/app",
            "health": "healthy",
            "running_service_count": 1,
            "service_count": 1,
            "cpu_percent": 0.5,
            "memory_bytes": 1024,
            "recent_warning": {"message": "hot"}
        }]);
        let warnings = json!([{
            "id": "warn_1",
            "rule": "cpu_spike",
            "severity": "warning",
            "message": "hot",
            "project_id": "prj_1",
            "service_id": "svc_1",
            "related_events": ["evt_1"]
        }]);
        let events = json!([{
            "id": "evt_1",
            "at": "2026-08-18T00:00:00Z",
            "project_id": "prj_1",
            "kind": {"type": "service_started", "service_id": "svc_1", "pid": 1}
        }]);

        let now = compact_now(&status, &projects, &warnings, &events);
        assert_eq!(now["ok"], true);
        assert!(now.get("platform").is_none());
        assert_eq!(now["docker"], false);
        assert_eq!(now["projects"][0]["warning"], "hot");
        assert!(now["warnings"][0].get("related_events").is_none());
        assert_eq!(now["events"][0]["kind"]["type"], "service_started");
    }

    #[test]
    fn strip_heavy_removes_sparklines() {
        let mut body = json!({
            "services": [{"id": "svc_1", "resource_history": [1, 2, 3], "name": "web"}]
        });
        body = strip_heavy(body);
        assert!(body["services"][0].get("resource_history").is_none());
        assert_eq!(body["services"][0]["name"], "web");
    }

    #[test]
    fn here_keeps_the_enclosing_project() {
        let cwd = std::env::current_dir().expect("cwd");
        let projects = json!([
            {"id": "me", "root": cwd},
            {"id": "other", "root": "/tmp/not-this"}
        ]);
        let filtered = filter_projects_here(projects, &cwd);
        let rows = filtered.as_array().expect("array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "me");
    }

    #[test]
    fn here_drops_projects_that_do_not_enclose_cwd() {
        let filtered = filter_projects_here(
            json!([{"id": "x", "root": "/no/such/devpulse-project-root"}]),
            Path::new("/tmp"),
        );
        assert!(filtered.as_array().expect("array").is_empty());
    }

    #[test]
    fn watch_types_split_and_default() {
        assert_eq!(parse_watch_types("events, warnings_changed").len(), 2);
        assert_eq!(parse_watch_types("").len(), 4);
    }
}
