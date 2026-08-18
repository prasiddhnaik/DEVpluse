//! HTTP API contract tests (tasks T3.2, T3.3).
//!
//! The state is seeded directly rather than collected from the machine the
//! tests happen to run on: the contract is what is under test here, not
//! discovery. `tests/daemon.rs` covers the real loop.

mod support;

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use devpulse_core::model::{Connection, Evidence, EvidenceType};
use devpulse_core::registry::RegistryDelta;
use devpulse_core::topology::TopologyDelta;
use devpulse_server::api;
use devpulse_server::security::OriginPolicy;
use devpulse_server::state::AppState;
use serde_json::Value;
use support::{at, project, service, tick};
use tower::ServiceExt;

/// A state holding one project, two services and the edge between them.
async fn seeded_state() -> AppState {
    let state = AppState::docker_unknown();
    let web = service("web", 41010, 100);
    let api = service("api", 41011, 200);
    let edge = Connection::new(
        web.id.clone(),
        api.id.clone(),
        41011,
        Evidence::observed(EvidenceType::ObservedSocket, at(5)),
    );

    let mut projects = BTreeMap::new();
    let project = project();
    projects.insert(project.id.clone(), project);

    state
        .apply_tick(
            &tick(RegistryDelta::default(), TopologyDelta::default()),
            &projects,
            vec![web, api],
            vec![edge],
            Vec::new(),
        )
        .await;
    state
}

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    request(
        state,
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request"),
    )
    .await
}

async fn request(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let response = api::router(state.clone(), OriginPolicy::default())
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

#[tokio::test]
async fn status_answers_before_anything_has_been_collected() {
    let (status, body) = get(&AppState::docker_unknown(), "/api/v1/status").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["platform"]["os"], std::env::consts::OS);
    assert_eq!(body["counts"]["services"], 0);
    assert_eq!(body["docker"]["available"], false);
    assert!(body["version"].is_string());
    assert!(body["started_at"].as_str().expect("rfc3339").ends_with('Z'));
}

#[tokio::test]
async fn projects_summarise_their_services() {
    let state = seeded_state().await;
    let (status, body) = get(&state, "/api/v1/projects").await;

    assert_eq!(status, StatusCode::OK);
    let projects = body.as_array().expect("array");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["name"], "devpulse-fixture");
    assert_eq!(projects[0]["service_count"], 2);
    assert_eq!(projects[0]["running_service_count"], 2);
    assert_eq!(projects[0]["health"], "healthy");
    assert_eq!(projects[0]["recent_warning"], Value::Null);
}

#[tokio::test]
async fn project_detail_carries_services_edges_and_events() {
    let state = seeded_state().await;
    let id = project().id.to_string();
    let (status, body) = get(&state, &format!("/api/v1/projects/{id}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["project"]["id"], id);
    assert_eq!(body["services"].as_array().expect("services").len(), 2);
    assert_eq!(body["connections"].as_array().expect("edges").len(), 1);
    assert!(body["warnings"].as_array().expect("warnings").is_empty());
    assert!(body["recent_events"].is_array());
}

#[tokio::test]
async fn service_detail_splits_connections_by_direction() {
    let state = seeded_state().await;
    let web = service("web", 41010, 100).id.to_string();
    let (status, body) = get(&state, &format!("/api/v1/services/{web}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "web");
    assert_eq!(body["kind"]["kind"], "host_process");
    assert_eq!(
        body["connections"]["outbound"]
            .as_array()
            .expect("out")
            .len(),
        1
    );
    assert!(
        body["connections"]["inbound"]
            .as_array()
            .expect("in")
            .is_empty()
    );
    assert_eq!(body["endpoints"][0]["port"], 41010);
}

#[tokio::test]
async fn every_edge_carries_evidence() {
    let state = seeded_state().await;
    let id = project().id.to_string();
    let (status, body) = get(&state, &format!("/api/v1/graph/{id}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nodes"].as_array().expect("nodes").len(), 2);

    let edges = body["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 1);
    let evidence = &edges[0]["evidence"];
    assert_eq!(evidence["evidence_type"], "observed_socket");
    assert_eq!(evidence["confidence"], 1.0);
    assert!(evidence["first_seen"].is_string());
    assert!(evidence["last_seen"].is_string());
}

#[tokio::test]
async fn unknown_ids_are_not_found_with_the_contract_error_shape() {
    let state = seeded_state().await;

    for uri in [
        "/api/v1/projects/prj_doesnotexist",
        "/api/v1/services/svc_doesnotexist",
        "/api/v1/graph/prj_doesnotexist",
    ] {
        let (status, body) = get(&state, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(body["error"]["code"], "not_found", "{uri}");
        assert!(body["error"]["message"].is_string(), "{uri}");
    }
}

#[tokio::test]
async fn a_malformed_since_is_rejected_rather_than_guessed() {
    let state = seeded_state().await;
    let (status, body) = get(&state, "/api/v1/events?since=yesterday").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn an_oversized_limit_is_clamped_not_rejected() {
    let state = seeded_state().await;
    let (status, _) = get(&state, "/api/v1/events?limit=100000").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_foreign_origin_gets_no_answer() {
    let state = seeded_state().await;
    let req = Request::builder()
        .uri("/api/v1/status")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .expect("request");

    let (status, body) = request(&state, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn the_dashboard_origin_is_allowed() {
    let state = seeded_state().await;
    let req = Request::builder()
        .uri("/api/v1/status")
        .header(header::ORIGIN, "http://localhost:3000")
        .body(Body::empty())
        .expect("request");

    let (status, _) = request(&state, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_api_exposes_no_mutating_route() {
    let state = seeded_state().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/status")
        .body(Body::empty())
        .expect("request");

    let (status, _) = request(&state, req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}
