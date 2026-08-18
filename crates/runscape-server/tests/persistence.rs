//! History survives a restart (tasks T5.1, T5.2), and nothing else does.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runscape_core::ids::{EventId, ProjectId, ServiceId};
use runscape_core::model::{EventKind, RunscapeEvent};
use runscape_server::persistence::{Persistence, TickWrite, restore};
use runscape_storage::{RetentionPolicy, Store};
use tempfile::TempDir;

mod support;

use support::{at, project, service};

fn event(secs: u64, sequence: u32) -> RunscapeEvent {
    RunscapeEvent {
        id: EventId::new(1_700_000_000_000 + secs * 1_000, sequence),
        at: at(secs),
        project_id: Some(project().id),
        kind: EventKind::ServiceStarted {
            service_id: service("web", 41010, 100).id,
            pid: Some(100),
        },
    }
}

/// The writer runs on a blocking thread, so a test has to wait for the write
/// to land rather than assume it did.
async fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(value) = check() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the write never landed");
}

#[tokio::test]
async fn a_tick_is_written_and_read_back() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("runscape.db");

    let persistence = Persistence::open(&path, RetentionPolicy::default()).expect("opens");
    persistence.write(TickWrite {
        projects: vec![project()],
        services: vec![service("web", 41010, 100)],
        events: vec![event(1, 1), event(2, 2)],
        samples: Vec::new(),
        warnings: Vec::new(),
    });

    let history = wait_for(|| {
        let history = restore(&path, 100).expect("restores");
        (history.events.len() == 2).then_some(history)
    })
    .await;

    assert_eq!(history.projects.len(), 1);
    // Oldest first: the ring is filled in order.
    assert_eq!(history.events[0].at, at(1));
    assert_eq!(history.events[1].at, at(2));
}

#[tokio::test]
async fn services_are_stored_but_not_restored_into_the_live_view() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("runscape.db");

    let persistence = Persistence::open(&path, RetentionPolicy::default()).expect("opens");
    persistence.write(TickWrite {
        projects: vec![project()],
        services: vec![service("web", 41010, 100)],
        events: vec![event(1, 1)],
        ..TickWrite::default()
    });

    wait_for(|| {
        let history = restore(&path, 100).expect("restores");
        (!history.events.is_empty()).then_some(())
    })
    .await;

    // The row exists…
    let store = Store::open(&path).expect("opens");
    assert_eq!(store.services(None).expect("services").len(), 1);

    // …but restoring deliberately does not hand it back: its PIDs are stale.
    let history = restore(&path, 100).expect("restores");
    assert!(
        history.projects.is_empty() || history.projects.len() == 1,
        "projects may be restored"
    );
    let restored_fields = format!("{history:?}");
    assert!(
        !restored_fields.contains("ProcessInstance"),
        "a restored history must not carry process instances: {restored_fields}"
    );
}

#[tokio::test]
async fn resource_samples_are_bounded_by_retention() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("runscape.db");

    let policy = RetentionPolicy {
        resource_sample_max_age: Duration::from_secs(60),
        max_resource_samples_per_service: 10,
        ..RetentionPolicy::default()
    };

    let service_id = ServiceId::derived("web");
    let store = Store::open(&path).expect("opens");
    for i in 0..50u64 {
        store
            .record_resource_samples(&[(
                service_id.clone(),
                runscape_core::model::ResourceSample {
                    at: SystemTime::now() - Duration::from_secs(50 - i),
                    cpu_percent: 1.0,
                    memory_bytes: 1024,
                    virtual_memory_bytes: 2048,
                    thread_count: 4,
                    disk_read_bytes: 0,
                    disk_write_bytes: 0,
                    connection_count: 0,
                },
            )])
            .expect("records");
    }
    drop(store);

    let persistence = Persistence::open(&path, policy).expect("opens");
    persistence.retention(SystemTime::now());

    let remaining = wait_for(|| {
        let store = Store::open(&path).expect("opens");
        let count = store
            .resource_samples(&service_id, 1_000)
            .expect("samples")
            .len();
        (count <= 10).then_some(count)
    })
    .await;

    assert!(
        remaining <= 10,
        "retention must cap per-service samples, found {remaining}"
    );
}

#[tokio::test]
async fn an_unwritable_database_does_not_stop_the_daemon() {
    // A directory where the file should be: opening must fail, and the caller
    // must be able to carry on without history.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("runscape.db");
    std::fs::create_dir(&path).expect("create the blocking directory");

    assert!(Persistence::open(&path, RetentionPolicy::default()).is_err());
    assert!(restore(&path, 10).is_err());
}

#[tokio::test]
async fn events_older_than_the_window_are_dropped() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("runscape.db");

    let store = Store::open(&path).expect("opens");
    let ancient = RunscapeEvent {
        id: EventId::new(1_000, 1),
        at: UNIX_EPOCH + Duration::from_secs(1),
        project_id: Some(ProjectId::derived("/tmp/old")),
        kind: EventKind::ServiceStopped {
            service_id: ServiceId::derived("old"),
            pid: Some(1),
        },
    };
    store.record_events(&[ancient]).expect("records");
    drop(store);

    let persistence = Persistence::open(&path, RetentionPolicy::default()).expect("opens");
    persistence.retention(SystemTime::now());

    wait_for(|| {
        let store = Store::open(&path).expect("opens");
        let events = store.recent_events(None, 100).expect("events");
        events.is_empty().then_some(())
    })
    .await;
}
