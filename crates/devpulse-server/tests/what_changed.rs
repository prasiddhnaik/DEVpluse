//! "What changed?" over the API (Milestone 7): warnings the rules fired, and
//! the context of a single event.

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use devpulse_core::ids::{EventId, ServiceId};
use devpulse_core::model::{DevPulseEvent, EventKind, Health};
use devpulse_core::registry::RegistryDelta;
use devpulse_core::topology::TopologyDelta;
use devpulse_events::warnings::{WarningEngine, WarningRules};
use devpulse_server::api;
use devpulse_server::security::OriginPolicy;
use devpulse_server::state::{AppState, TickUpdate};
use serde_json::Value;
use support::{at, project, service, tick};
use tower::ServiceExt;

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let response = api::router(state.clone(), OriginPolicy::default())
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

fn file_changed(secs: u64, sequence: u32) -> DevPulseEvent {
    DevPulseEvent {
        id: EventId::new(1_700_000_000_000 + secs * 1_000, sequence),
        at: at(secs),
        project_id: Some(project().id),
        kind: EventKind::FileChanged {
            project_id: project().id,
            path: "/tmp/devpulse-fixture/src/main.rs".into(),
        },
    }
}

fn restarted(secs: u64, sequence: u32, service_id: &ServiceId) -> DevPulseEvent {
    DevPulseEvent {
        id: EventId::new(1_700_000_000_000 + secs * 1_000, sequence),
        at: at(secs),
        project_id: Some(project().id),
        kind: EventKind::ServiceRestarted {
            service_id: service_id.clone(),
            old_pid: Some(100),
            new_pid: Some(200),
        },
    }
}

fn health_changed(secs: u64, sequence: u32, service_id: &ServiceId) -> DevPulseEvent {
    DevPulseEvent {
        id: EventId::new(1_700_000_000_000 + secs * 1_000, sequence),
        at: at(secs),
        project_id: Some(project().id),
        kind: EventKind::HealthChanged {
            service_id: service_id.clone(),
            from: Health::Healthy,
            to: Health::Degraded,
        },
    }
}

/// The story from `TASKS.md` T7.2, in the state the API reads from.
async fn state_with_the_story() -> (AppState, DevPulseEvent) {
    let state = AppState::docker_unknown();
    let web = service("web", 41010, 100);
    let restart = restarted(2, 2, &web.id);

    let mut projects = BTreeMap::new();
    let project = project();
    projects.insert(project.id.clone(), project);

    state
        .apply_tick(TickUpdate {
            tick: &tick(RegistryDelta::default(), TopologyDelta::default()),
            projects: &projects,
            services: vec![web.clone()],
            connections: Vec::new(),
            events: vec![
                file_changed(0, 1),
                restart.clone(),
                health_changed(7, 3, &web.id),
            ],
            warnings: None,
        })
        .await;

    (state, restart)
}

#[tokio::test]
async fn an_events_context_reads_as_a_story() {
    let (state, restart) = state_with_the_story().await;
    let (status, body) = get(
        &state,
        &format!("/api/v1/events/{}/context", restart.id.as_str()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["event"]["id"], restart.id.as_str());

    let before = body["before"].as_array().expect("before");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["relation"], "preceding_file_change");
    assert_eq!(before[0]["offset_ms"], -2000);
    assert_eq!(before[0]["kind"]["type"], "file_changed");

    let after = body["after"].as_array().expect("after");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["relation"], "same_service");
    assert_eq!(after[0]["offset_ms"], 5000);
}

#[tokio::test]
async fn a_narrow_window_excludes_what_falls_outside_it() {
    let (state, restart) = state_with_the_story().await;
    let (status, body) = get(
        &state,
        &format!(
            "/api/v1/events/{}/context?window_ms=3000",
            restart.id.as_str()
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["window_ms"], 3000);
    assert_eq!(body["before"].as_array().expect("before").len(), 1);
    assert!(
        body["after"].as_array().expect("after").is_empty(),
        "the health change is five seconds out"
    );
}

#[tokio::test]
async fn an_unknown_event_has_no_context() {
    let (state, _) = state_with_the_story().await;
    let (status, body) = get(&state, "/api/v1/events/evt_nope/context").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn a_flapping_service_produces_a_warning_on_the_wire() {
    let state = AppState::docker_unknown();
    let web = service("web", 41010, 100);
    let mut engine = WarningEngine::new(WarningRules {
        restart_threshold: 3,
        restart_window: Duration::from_secs(60),
        ..WarningRules::default()
    });

    let mut projects = BTreeMap::new();
    let project = project();
    projects.insert(project.id.clone(), project);

    let mut tick_result = tick(RegistryDelta::default(), TopologyDelta::default());
    tick_result.at = Some(at(40));

    let applied = state
        .apply_tick(TickUpdate {
            tick: &tick_result,
            projects: &projects,
            services: vec![web.clone()],
            connections: Vec::new(),
            events: vec![
                restarted(10, 1, &web.id),
                restarted(20, 2, &web.id),
                restarted(30, 3, &web.id),
            ],
            warnings: Some(&mut engine),
        })
        .await;

    assert_eq!(applied.warnings.len(), 1, "the rule fired once");
    assert!(
        applied
            .frames
            .iter()
            .any(|frame| frame.contains("warnings_changed")),
        "a client must be told a warning appeared"
    );

    let (status, body) = get(&state, "/api/v1/warnings").await;
    assert_eq!(status, StatusCode::OK);
    let warnings = body.as_array().expect("warnings");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["rule"], "restart_loop");
    assert_eq!(warnings[0]["severity"], "critical");
    assert_eq!(
        warnings[0]["related_events"]
            .as_array()
            .expect("related")
            .len(),
        3,
        "a warning cites the events it fired on"
    );

    // The project view carries it too, as the most recent warning.
    let (_, project_body) = get(
        &state,
        &format!("/api/v1/projects/{}", support::project().id),
    )
    .await;
    assert_eq!(
        project_body["warnings"].as_array().expect("warnings").len(),
        1
    );
    assert_eq!(
        project_body["project"]["recent_warning"]["rule"],
        "restart_loop"
    );
}

#[tokio::test]
async fn a_warning_that_stops_being_true_disappears() {
    let state = AppState::docker_unknown();
    let web = service("web", 41010, 100);
    let mut engine = WarningEngine::new(WarningRules::default());

    let mut projects = BTreeMap::new();
    let project = project();
    projects.insert(project.id.clone(), project);

    let mut first = tick(RegistryDelta::default(), TopologyDelta::default());
    first.at = Some(at(40));
    state
        .apply_tick(TickUpdate {
            tick: &first,
            projects: &projects,
            services: vec![web.clone()],
            connections: Vec::new(),
            events: vec![
                restarted(10, 1, &web.id),
                restarted(20, 2, &web.id),
                restarted(30, 3, &web.id),
            ],
            warnings: Some(&mut engine),
        })
        .await;

    // Ten minutes later the restarts are outside the window; nothing new fires.
    let mut later = tick(RegistryDelta::default(), TopologyDelta::default());
    later.at = Some(at(640));
    let applied = state
        .apply_tick(TickUpdate {
            tick: &later,
            projects: &projects,
            services: vec![web],
            connections: Vec::new(),
            events: Vec::new(),
            warnings: Some(&mut engine),
        })
        .await;

    assert!(applied.warnings.is_empty());
    let (_, body) = get(&state, "/api/v1/warnings").await;
    assert!(body.as_array().expect("warnings").is_empty());
}
