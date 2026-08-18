//! Containers reach the registry through the same path as host processes
//! (task T6.3), including when Docker is absent or answering with errors.

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use runscape_core::identity::Runtime;
use runscape_core::model::ServiceKind;
use runscape_docker::collector::ContainerCollector;
use runscape_docker::container::{ContainerPort, ContainerSnapshot, ContainerState};
use runscape_docker::error::DockerError;
use runscape_docker::{ContainerIdentity, ObservedContainer};
use runscape_server::snapshot::{SnapshotConfig, SnapshotLoop};

/// A collector that answers from a script rather than from Docker.
struct FakeDocker {
    snapshot: Option<ContainerSnapshot>,
}

#[async_trait]
impl ContainerCollector for FakeDocker {
    async fn snapshot(&self) -> Result<ContainerSnapshot, DockerError> {
        match &self.snapshot {
            Some(snapshot) => Ok(snapshot.clone()),
            None => Err(DockerError::Api {
                source: bollard::errors::Error::DockerResponseServerError {
                    status_code: 500,
                    message: "the docker daemon went away".to_string(),
                },
            }),
        }
    }
}

fn container(name: &str, service: &str, state: ContainerState, port: u16) -> ObservedContainer {
    ObservedContainer {
        id: format!("{name}-id"),
        identity: ContainerIdentity {
            name: name.to_string(),
            compose_project: Some("fixture-stack".to_string()),
            compose_service: Some(service.to_string()),
        },
        image: "postgres:16".to_string(),
        state,
        status: "Up 3 minutes".to_string(),
        ports: vec![ContainerPort {
            private_port: port,
            public_port: Some(port),
            protocol: runscape_core::Protocol::Tcp,
            host_ip: Some("127.0.0.1".parse().expect("ip")),
        }],
        networks: vec!["fixture-stack_default".to_string()],
        cpu_percent: None,
        memory_bytes: None,
    }
}

fn snapshot(containers: Vec<ObservedContainer>) -> ContainerSnapshot {
    ContainerSnapshot {
        captured_at: SystemTime::now(),
        duration: Duration::from_millis(7),
        containers,
    }
}

async fn tick_with(collector: FakeDocker) -> SnapshotLoop {
    let mut loop_ =
        SnapshotLoop::new(SnapshotConfig::default()).with_containers(Box::new(collector));
    loop_.tick().await.expect("tick succeeds");
    loop_
}

#[tokio::test]
async fn a_running_container_becomes_a_service_in_its_compose_project() {
    let loop_ = tick_with(FakeDocker {
        snapshot: Some(snapshot(vec![container(
            "fixture-db-1",
            "db",
            ContainerState::Running,
            5432,
        )])),
    })
    .await;

    let service = loop_
        .registry()
        .services()
        .find(|s| matches!(s.kind, ServiceKind::Container(_)))
        .expect("the container is a service");

    assert_eq!(service.name, "db", "the compose service name wins");
    assert_eq!(service.runtime, Runtime::Container);
    assert!(
        service.instances.is_empty(),
        "a container has no host PIDs to report"
    );
    assert!(service.is_running(), "liveness comes from health, not PIDs");
    assert_eq!(service.endpoints.len(), 1);
    assert_eq!(service.endpoints[0].port, 5432);

    let project = service
        .project_id
        .as_ref()
        .expect("grouped by compose label");
    let project = loop_
        .projects()
        .get(project)
        .expect("project is remembered");
    assert_eq!(project.name, "fixture-stack");
}

#[tokio::test]
async fn a_stopped_container_is_not_a_running_service() {
    let loop_ = tick_with(FakeDocker {
        snapshot: Some(snapshot(vec![container(
            "fixture-api-1",
            "api",
            ContainerState::Exited,
            8080,
        )])),
    })
    .await;

    assert!(
        !loop_
            .registry()
            .services()
            .any(|s| matches!(s.kind, ServiceKind::Container(_))),
        "an exited container must not be reported as a service"
    );
}

#[tokio::test]
async fn a_container_that_stays_up_does_not_look_like_it_restarts() {
    let collector = FakeDocker {
        snapshot: Some(snapshot(vec![container(
            "fixture-db-1",
            "db",
            ContainerState::Running,
            5432,
        )])),
    };
    let mut loop_ =
        SnapshotLoop::new(SnapshotConfig::default()).with_containers(Box::new(collector));

    loop_.tick().await.expect("first tick");
    let second = loop_.tick().await.expect("second tick");

    let service = loop_
        .registry()
        .services()
        .find(|s| matches!(s.kind, ServiceKind::Container(_)))
        .expect("still a service");

    // Real host processes on the machine may legitimately restart between the
    // two ticks; the container is what this asserts about.
    assert!(
        !second
            .registry_delta
            .restarted
            .iter()
            .any(|(id, _, _)| id == &service.id),
        "a container with no PIDs must not be diffed as a PID change: {:?}",
        second.registry_delta.restarted
    );
    assert_eq!(service.restart_count, 0);
}

#[tokio::test]
async fn docker_failing_degrades_to_host_processes() {
    let loop_ = tick_with(FakeDocker { snapshot: None }).await;

    // The tick still succeeded; it simply has no containers in it.
    assert!(
        !loop_
            .registry()
            .services()
            .any(|s| matches!(s.kind, ServiceKind::Container(_)))
    );
}

#[tokio::test]
async fn the_status_reports_why_docker_produced_nothing() {
    let mut loop_ = SnapshotLoop::new(SnapshotConfig::default())
        .with_containers(Box::new(FakeDocker { snapshot: None }));
    let tick = loop_.tick().await.expect("tick succeeds");

    let container = tick.container.expect("a collector exists, so it reports");
    assert!(
        container
            .error
            .expect("the reason is reported")
            .contains("daemon"),
        "a developer must be able to see why the container list is empty"
    );
}

#[tokio::test]
async fn no_docker_means_no_container_collector_status() {
    let mut loop_ = SnapshotLoop::new(SnapshotConfig::default());
    let tick = loop_.tick().await.expect("tick succeeds");

    assert!(!loop_.inspects_containers());
    assert!(
        tick.container.is_none(),
        "a machine without Docker reports nothing about Docker"
    );
}
