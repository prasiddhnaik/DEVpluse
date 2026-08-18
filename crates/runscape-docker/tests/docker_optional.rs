//! Milestone 6 integration scenario.
//!
//! This test is the acceptance criterion for "Docker is optional": it must pass
//! on a machine with Docker running and on a machine with no Docker at all. It
//! therefore branches on the detected state instead of assuming one, and asserts
//! something real in both branches.

use runscape_docker::{ContainerCollector, DockerAvailability};

#[tokio::test]
async fn docker_is_optional_and_a_snapshot_measures_itself() {
    match DockerAvailability::detect().await {
        DockerAvailability::Unavailable { reason } => {
            assert!(
                !reason.trim().is_empty(),
                "an unavailable daemon must explain itself"
            );
            eprintln!("skipped: docker unavailable: {reason}");
        }
        DockerAvailability::Available(collector) => {
            assert!(
                !collector.stats_enabled(),
                "stats must be off until asked for"
            );

            let snapshot = collector.snapshot().await.expect("snapshot a live daemon");

            assert!(
                snapshot.duration.as_nanos() > 0,
                "collector duration must be measured (AGENTS.md rule 7)"
            );
            assert!(snapshot.captured_at <= std::time::SystemTime::now());

            for container in &snapshot.containers {
                assert!(
                    !container.identity.name.is_empty(),
                    "every container needs an identity name"
                );
                assert_eq!(
                    container.cpu_percent, None,
                    "stats are off, so nothing may be reported"
                );
                assert_eq!(container.memory_bytes, None);

                let service = container.to_service(None, snapshot.captured_at);
                assert!(!service.fingerprint.is_empty());
                assert!(service.instances.is_empty());
            }

            eprintln!(
                "docker reachable: {} containers in {:?}",
                snapshot.containers.len(),
                snapshot.duration
            );
        }
    }
}
