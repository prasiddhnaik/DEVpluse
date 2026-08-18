//! `WS /ws/v1` contract tests (task T3.4).
//!
//! Acceptance for T3.4 is "a test client receives service start/stop changes",
//! so this drives the state the way the snapshot loop does and asserts on what
//! arrives at a real socket.

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use devpulse_core::ids::EventId;
use devpulse_core::model::{DevPulseEvent, EventKind, Project};
use devpulse_core::registry::RegistryDelta;
use devpulse_core::topology::TopologyDelta;
use devpulse_server::api;
use devpulse_server::security::OriginPolicy;
use devpulse_server::state::AppState;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use support::{at, project, service, tick};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::ORIGIN;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// Frames must arrive promptly; a hung socket should fail the test, not hang it.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

struct TestServer {
    state: AppState,
    url: String,
}

/// A server with no snapshot loop: the tests drive the state themselves, so
/// nothing depends on what is running on the machine.
async fn start() -> TestServer {
    let state = AppState::docker_unknown();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let router = api::router(state.clone(), OriginPolicy::default());

    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    TestServer {
        state,
        url: format!("ws://{addr}/ws/v1"),
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(server: &TestServer) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(server.url.as_str())
        .await
        .expect("websocket connects");
    socket
}

async fn next_frame(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(FRAME_TIMEOUT, socket.next())
            .await
            .expect("a frame arrives before the timeout")
            .expect("stream is open")
            .expect("frame reads");

        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("frames are json"),
            // Control frames are not part of the contract; keep reading.
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

fn projects() -> BTreeMap<devpulse_core::ids::ProjectId, Project> {
    let project = project();
    BTreeMap::from([(project.id.clone(), project)])
}

/// One service starts, with the event the deriver would have produced.
async fn publish_start(state: &AppState) {
    let web = service("web", 41010, 100);
    let delta = RegistryDelta {
        started: vec![(web.id.clone(), 100)],
        ..RegistryDelta::default()
    };
    let event = DevPulseEvent {
        id: EventId::new(1_700_000_010_000, 1),
        at: at(10),
        project_id: web.project_id.clone(),
        kind: EventKind::ServiceStarted {
            service_id: web.id.clone(),
            pid: 100,
        },
    };

    let frames = state
        .apply_tick(
            &tick(delta, TopologyDelta::default()),
            &projects(),
            vec![web],
            Vec::new(),
            vec![event],
        )
        .await;
    for frame in frames {
        state.publish(frame);
    }
}

/// The same service stops: no instances, and it leaves the registry.
async fn publish_stop(state: &AppState) {
    let web = service("web", 41010, 100);
    let delta = RegistryDelta {
        stopped: vec![(web.id.clone(), 100)],
        ..RegistryDelta::default()
    };
    let mut stopped = web.clone();
    stopped.instances.clear();
    stopped.health = devpulse_core::model::Health::Stopped;

    let frames = state
        .apply_tick(
            &tick(delta, TopologyDelta::default()),
            &projects(),
            vec![stopped],
            Vec::new(),
            Vec::new(),
        )
        .await;
    for frame in frames {
        state.publish(frame);
    }
}

#[tokio::test]
async fn a_client_gets_exactly_one_snapshot_on_connect() {
    let server = start().await;
    let mut socket = connect(&server).await;

    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["type"], "snapshot");
    assert!(frame["status"]["version"].is_string());
    assert!(frame["projects"].is_array());
    assert!(frame["services"].is_array());
    assert!(frame["connections"].is_array());
    assert!(frame["warnings"].is_array());

    // Nothing else is sent until something changes.
    let quiet = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
    assert!(quiet.is_err(), "an idle daemon must not chatter");
}

#[tokio::test]
async fn a_client_sees_a_service_start_and_stop() {
    let server = start().await;
    let mut socket = connect(&server).await;
    assert_eq!(next_frame(&mut socket).await["type"], "snapshot");

    publish_start(&server.state).await;

    let changed = next_frame(&mut socket).await;
    assert_eq!(changed["type"], "services_changed");
    assert_eq!(changed["services"][0]["name"], "web");
    assert_eq!(changed["services"][0]["health"], "healthy");

    let events = next_frame(&mut socket).await;
    assert_eq!(events["type"], "events");
    assert_eq!(events["events"][0]["kind"]["type"], "service_started");

    publish_stop(&server.state).await;

    let stopped = next_frame(&mut socket).await;
    assert_eq!(stopped["type"], "services_changed");
    assert_eq!(stopped["services"][0]["health"], "stopped");
    assert!(
        stopped["services"][0]["instances"]
            .as_array()
            .expect("instances")
            .is_empty()
    );
}

#[tokio::test]
async fn resnapshot_returns_the_current_world() {
    let server = start().await;
    let mut socket = connect(&server).await;
    assert_eq!(next_frame(&mut socket).await["type"], "snapshot");

    publish_start(&server.state).await;
    assert_eq!(next_frame(&mut socket).await["type"], "services_changed");
    assert_eq!(next_frame(&mut socket).await["type"], "events");

    socket
        .send(Message::Text(r#"{"type":"resnapshot"}"#.into()))
        .await
        .expect("send resnapshot");

    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["type"], "snapshot");
    assert_eq!(frame["services"].as_array().expect("services").len(), 1);
    assert_eq!(frame["status"]["counts"]["services"], 1);
}

#[tokio::test]
async fn unknown_client_frames_are_ignored_not_obeyed() {
    let server = start().await;
    let mut socket = connect(&server).await;
    assert_eq!(next_frame(&mut socket).await["type"], "snapshot");

    for junk in [r#"{"type":"kill","pid":1}"#, "not json at all", "{}"] {
        socket
            .send(Message::Text(junk.into()))
            .await
            .expect("send junk");
    }

    // The socket still works, and the junk produced no frames of its own.
    publish_start(&server.state).await;
    assert_eq!(next_frame(&mut socket).await["type"], "services_changed");
}

#[tokio::test]
async fn a_foreign_origin_cannot_upgrade() {
    let server = start().await;
    let mut request = server.url.as_str().into_client_request().expect("request");
    request
        .headers_mut()
        .insert(ORIGIN, "https://evil.example".parse().expect("header"));

    let result = tokio_tungstenite::connect_async(request).await;
    match result {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 403),
        Err(other) => panic!("expected an HTTP rejection, got {other:?}"),
        Ok(_) => panic!("a foreign origin must not be able to open the socket"),
    }
}
