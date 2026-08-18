//! Behavioural tests for the persistence layer.
//!
//! These exercise the API the daemon actually calls, through a real SQLite
//! database (`tempfile` on disk where durability is the point, in-memory
//! otherwise). Nothing here asserts on SQL text: the contract is "what you wrote
//! is what you read back", and every assertion is written to fail if a column,
//! an encoding, or a retention bound is wrong.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runscape_core::model::{
    Endpoint, EventKind, Health, ProcessInstance, Project, Protocol, ResourceSample, RunscapeEvent,
    Service, ServiceKind, Severity, Warning,
};
use runscape_core::project::{ProjectEvidence, ProjectMarker, RootKind};
use runscape_core::{
    ConnectionId, ContainerIdentity, EventId, ProjectId, Runtime, ServiceFingerprint, ServiceId,
};
use runscape_storage::{RetentionPolicy, Store};

/// Timestamps are stored to millisecond precision, so fixtures are built on
/// exact milliseconds; using `SystemTime::now()` would make every equality
/// assertion a coin flip on the nanosecond remainder.
fn at(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

fn project(root: &str, name: &str) -> Project {
    let root = PathBuf::from(root);
    Project {
        id: ProjectId::derived(&root.to_string_lossy()),
        name: name.to_owned(),
        kind: RootKind::Workspace,
        confidence: 0.95,
        evidence: vec![
            ProjectEvidence::GitRoot { path: root.clone() },
            ProjectEvidence::WorkspaceRoot {
                path: root.clone(),
                marker: ProjectMarker::PnpmWorkspace,
            },
        ],
        first_seen: at(1_000),
        last_seen: at(2_000),
        root,
    }
}

fn host_service(project: &Project, name: &str, port: u16) -> Service {
    let cwd = project.root.join(name);
    let executable = PathBuf::from("/usr/local/bin/node");
    let fingerprint = ServiceFingerprint::host(
        Some(&project.id),
        Runtime::Node,
        Some(&executable),
        Some(&cwd),
        Some(port),
    );
    Service {
        id: fingerprint.service_id(),
        project_id: Some(project.id.clone()),
        name: name.to_owned(),
        kind: ServiceKind::HostProcess,
        runtime: Runtime::Node,
        fingerprint: fingerprint.canonical().to_owned(),
        health: Health::Healthy,
        instances: vec![ProcessInstance {
            pid: 4242,
            parent_pid: Some(1),
            executable: Some(executable),
            command: vec!["node".into(), "server.js".into()],
            cwd: Some(cwd),
            started_at_epoch_secs: 1_700_000_000,
            cpu_percent: 12.5,
            memory_bytes: 256 * 1024 * 1024,
            virtual_memory_bytes: 512 * 1024 * 1024,
            thread_count: Some(12),
            disk_read_bytes: 0,
            disk_write_bytes: 0,
        }],
        endpoints: vec![Endpoint {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            protocol: Protocol::Tcp,
            pid: Some(4242),
        }],
        first_seen: at(1_500),
        last_seen: at(2_500),
        restart_count: 3,
        measured_cpu_percent: None,
        measured_memory_bytes: None,
    }
}

fn container_service(project: &Project) -> Service {
    let identity = ContainerIdentity {
        name: "shop-postgres-1".to_owned(),
        compose_project: Some("shop".to_owned()),
        compose_service: Some("postgres".to_owned()),
    };
    let fingerprint = ServiceFingerprint::container(&identity);
    Service {
        id: fingerprint.service_id(),
        project_id: Some(project.id.clone()),
        name: "postgres".to_owned(),
        kind: ServiceKind::Container(identity),
        runtime: Runtime::Container,
        fingerprint: fingerprint.canonical().to_owned(),
        health: Health::Degraded,
        instances: Vec::new(),
        endpoints: vec![Endpoint {
            address: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 5432,
            protocol: Protocol::Tcp,
            pid: None,
        }],
        first_seen: at(900),
        last_seen: at(3_000),
        restart_count: 0,
        measured_cpu_percent: None,
        measured_memory_bytes: None,
    }
}

fn event(
    sequence: u32,
    millis: u64,
    project: Option<&ProjectId>,
    kind: EventKind,
) -> RunscapeEvent {
    RunscapeEvent {
        id: EventId::new(millis, sequence),
        at: at(millis),
        project_id: project.cloned(),
        kind,
    }
}

/// Every [`EventKind`] variant, so a new variant that cannot be encoded shows up
/// as a failing test rather than as a silently dropped row.
fn one_of_every_event_kind(project: &ProjectId, service: &ServiceId) -> Vec<RunscapeEvent> {
    let connection = ConnectionId::derived("svc_a->svc_b:5432");
    let kinds = vec![
        EventKind::ProjectDetected {
            project_id: project.clone(),
        },
        EventKind::ServiceStarted {
            service_id: service.clone(),
            pid: Some(10),
        },
        EventKind::ServiceStopped {
            service_id: service.clone(),
            pid: Some(10),
        },
        EventKind::ServiceRestarted {
            service_id: service.clone(),
            old_pid: Some(10),
            new_pid: Some(11),
        },
        EventKind::PortOpened {
            service_id: Some(service.clone()),
            port: 3000,
        },
        EventKind::PortClosed {
            service_id: None,
            port: 3000,
        },
        EventKind::ConnectionStarted {
            connection_id: connection.clone(),
            source: service.clone(),
            target: ServiceId::derived("target"),
            target_port: 5432,
        },
        EventKind::ConnectionEnded {
            connection_id: connection,
        },
        EventKind::HealthChanged {
            service_id: service.clone(),
            from: Health::Unknown,
            to: Health::Healthy,
        },
        EventKind::ResourceWarning {
            service_id: service.clone(),
            detail: "rss grew 40% in 5 minutes".to_owned(),
        },
        EventKind::FileChanged {
            project_id: project.clone(),
            path: PathBuf::from("/tmp/shop/src/index.ts"),
        },
    ];

    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let sequence = u32::try_from(index).expect("small index");
            event(sequence, 10_000 + u64::from(sequence), Some(project), kind)
        })
        .collect()
}

fn warning(id: &str, project: &ProjectId, service: &ServiceId, last_seen: u64) -> Warning {
    Warning {
        id: id.to_owned(),
        rule: "restart_loop".to_owned(),
        severity: Severity::Critical,
        project_id: Some(project.clone()),
        service_id: Some(service.clone()),
        message: "restarted 5 times in 60s".to_owned(),
        first_seen: at(4_000),
        last_seen: at(last_seen),
        related_events: vec![EventId::new(4_000, 1), EventId::new(4_100, 2)],
    }
}

fn seeded() -> (Store, Project, Service) {
    let store = Store::open_in_memory().expect("open in memory");
    let project = project("/tmp/shop", "shop");
    let service = host_service(&project, "api", 3000);
    store.upsert_project(&project).expect("upsert project");
    store.upsert_service(&service).expect("upsert service");
    (store, project, service)
}

#[test]
fn opening_an_existing_database_is_idempotent_and_preserves_data() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A nested path: first run must create the data directory itself.
    let path = dir.path().join("state/runscape.db");
    let project = project("/tmp/shop", "shop");

    {
        let store = Store::open(&path).expect("first open");
        store.upsert_project(&project).expect("upsert");
    }

    let reopened = Store::open(&path).expect("second open");
    assert_eq!(reopened.projects().expect("projects"), vec![project]);

    // A third open with the schema already at the current version must also be a
    // no-op rather than an error, and must not disturb the rows.
    let third = Store::open(&path).expect("third open");
    assert_eq!(third.projects().expect("projects").len(), 1);
}

#[test]
fn a_second_handle_on_the_same_file_sees_committed_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("runscape.db");
    let writer = Store::open(&path).expect("writer");
    let reader = Store::open(&path).expect("reader");

    writer
        .upsert_project(&project("/tmp/shop", "shop"))
        .expect("upsert");

    assert_eq!(reader.projects().expect("projects").len(), 1);
}

#[test]
fn projects_round_trip() {
    let store = Store::open_in_memory().expect("open");
    let project = project("/tmp/shop", "shop");
    store.upsert_project(&project).expect("upsert");

    assert_eq!(store.projects().expect("projects"), vec![project]);
}

#[test]
fn host_and_container_services_round_trip() {
    let (store, project, host) = seeded();
    let container = container_service(&project);
    store.upsert_service(&container).expect("upsert container");

    let stored = store.services(None).expect("services");
    // Ordered by name: "api" before "postgres".
    assert_eq!(stored, vec![host, container]);
}

#[test]
fn every_event_kind_round_trips() {
    let (store, project, service) = seeded();
    let events = one_of_every_event_kind(&project.id, &service.id);

    let recorded = store.record_events(&events).expect("record");
    assert_eq!(recorded, events.len());

    let mut stored = store
        .recent_events(None, events.len() * 2)
        .expect("recent events");
    stored.reverse(); // recent_events is newest first; fixtures are oldest first.
    assert_eq!(stored, events);
}

#[test]
fn warnings_round_trip() {
    let (store, project, service) = seeded();
    let warning = warning("restart_loop:api", &project.id, &service.id, 5_000);
    store.upsert_warning(&warning).expect("upsert");

    assert_eq!(store.warnings(None).expect("warnings"), vec![warning]);
}

#[test]
fn recent_events_returns_the_newest_first_and_honours_the_limit() {
    let (store, project, service) = seeded();
    let events: Vec<_> = (0..10)
        .map(|index| {
            event(
                index,
                1_000 + u64::from(index),
                Some(&project.id),
                EventKind::ServiceStarted {
                    service_id: service.id.clone(),
                    pid: Some(100 + index),
                },
            )
        })
        .collect();
    store.record_events(&events).expect("record");

    let newest = store.recent_events(None, 3).expect("recent");
    assert_eq!(newest.len(), 3);
    let ats: Vec<_> = newest.iter().map(|e| e.at).collect();
    assert_eq!(ats, vec![at(1_009), at(1_008), at(1_007)]);

    assert!(store.recent_events(None, 0).expect("zero limit").is_empty());
    assert_eq!(store.recent_events(None, 100).expect("all").len(), 10);
}

#[test]
fn reads_can_be_filtered_by_project() {
    let store = Store::open_in_memory().expect("open");
    let shop = project("/tmp/shop", "shop");
    let blog = project("/tmp/blog", "blog");
    store.upsert_project(&shop).expect("upsert shop");
    store.upsert_project(&blog).expect("upsert blog");

    let shop_api = host_service(&shop, "api", 3000);
    let blog_web = host_service(&blog, "web", 4000);
    store.upsert_service(&shop_api).expect("upsert shop api");
    store.upsert_service(&blog_web).expect("upsert blog web");

    store
        .record_events(&[
            event(
                0,
                1_000,
                Some(&shop.id),
                EventKind::PortOpened {
                    service_id: Some(shop_api.id.clone()),
                    port: 3000,
                },
            ),
            event(
                1,
                1_001,
                Some(&blog.id),
                EventKind::PortOpened {
                    service_id: Some(blog_web.id.clone()),
                    port: 4000,
                },
            ),
            // An event with no project must never leak into a filtered read.
            event(
                2,
                1_002,
                None,
                EventKind::PortClosed {
                    service_id: None,
                    port: 9999,
                },
            ),
        ])
        .expect("record");

    store
        .upsert_warning(&warning("w:shop", &shop.id, &shop_api.id, 5_000))
        .expect("shop warning");
    store
        .upsert_warning(&warning("w:blog", &blog.id, &blog_web.id, 6_000))
        .expect("blog warning");

    let services = store.services(Some(&shop.id)).expect("shop services");
    assert_eq!(services, vec![shop_api]);

    let events = store
        .recent_events(Some(&shop.id), 10)
        .expect("shop events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].project_id.as_ref(), Some(&shop.id));

    let warnings = store.warnings(Some(&blog.id)).expect("blog warnings");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].id, "w:blog");

    assert_eq!(store.recent_events(None, 10).expect("all events").len(), 3);
}

#[test]
fn warnings_are_ordered_by_most_recently_seen() {
    let (store, project, service) = seeded();
    store
        .upsert_warning(&warning("older", &project.id, &service.id, 5_000))
        .expect("older");
    store
        .upsert_warning(&warning("newer", &project.id, &service.id, 9_000))
        .expect("newer");

    let ids: Vec<_> = store
        .warnings(None)
        .expect("warnings")
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert_eq!(ids, vec!["newer", "older"]);
}

#[test]
fn upserts_keep_the_earliest_first_seen_and_the_latest_last_seen() {
    let (store, project, service) = seeded();

    let mut later = service.clone();
    later.first_seen = at(9_000);
    later.last_seen = at(9_000);
    later.health = Health::Stopped;
    store.upsert_service(&later).expect("re-upsert service");

    let stored = store.services(None).expect("services").remove(0);
    assert_eq!(stored.first_seen, service.first_seen, "first_seen kept");
    assert_eq!(stored.last_seen, at(9_000), "last_seen advanced");
    assert_eq!(stored.health, Health::Stopped, "latest state wins");

    let mut earlier = project.clone();
    earlier.first_seen = at(10);
    earlier.last_seen = at(20);
    store.upsert_project(&earlier).expect("re-upsert project");

    let stored = store.projects().expect("projects").remove(0);
    assert_eq!(stored.first_seen, at(10), "earlier first_seen wins");
    assert_eq!(stored.last_seen, project.last_seen, "later last_seen kept");
}

#[test]
fn re_recording_an_event_id_is_a_no_op() {
    let (store, project, service) = seeded();
    let events = vec![event(
        0,
        1_000,
        Some(&project.id),
        EventKind::ServiceStopped {
            service_id: service.id.clone(),
            pid: Some(7),
        },
    )];

    assert_eq!(store.record_events(&events).expect("first"), 1);
    assert_eq!(store.record_events(&events).expect("retry"), 0);
    assert_eq!(store.recent_events(None, 10).expect("events").len(), 1);
    assert_eq!(store.record_events(&[]).expect("empty"), 0);
}

#[test]
fn events_do_not_require_the_project_row_to_exist_yet() {
    let store = Store::open_in_memory().expect("open");
    let orphan = ProjectId::derived("/tmp/not-yet-written");

    let recorded = store
        .record_events(&[event(
            0,
            1_000,
            Some(&orphan),
            EventKind::ProjectDetected {
                project_id: orphan.clone(),
            },
        )])
        .expect("record");

    assert_eq!(recorded, 1);
    assert_eq!(
        store.recent_events(Some(&orphan), 10).expect("read").len(),
        1
    );
}

#[test]
fn a_service_cannot_reference_an_unknown_project() {
    let store = Store::open_in_memory().expect("open");
    let ghost = project("/tmp/ghost", "ghost");
    let service = host_service(&ghost, "api", 3000);

    // Proves `foreign_keys = ON` is actually in effect.
    let err = store
        .upsert_service(&service)
        .expect_err("foreign key must be enforced");
    assert!(
        err.to_string().to_lowercase().contains("sqlite"),
        "unexpected error: {err}"
    );
}

#[test]
fn aliases_are_upserted_not_duplicated() {
    let (store, _project, service) = seeded();

    assert_eq!(store.alias(&service.id).expect("missing alias"), None);
    store.set_alias(&service.id, "shop api").expect("set");
    assert_eq!(
        store.alias(&service.id).expect("alias"),
        Some("shop api".to_owned())
    );

    store.set_alias(&service.id, "the api").expect("overwrite");
    assert_eq!(
        store.alias(&service.id).expect("alias"),
        Some("the api".to_owned())
    );
}

#[test]
fn resource_samples_read_back_oldest_first_within_the_limit() {
    let (store, _project, service) = seeded();
    for index in 0..5u64 {
        store
            .record_resource_sample(
                &service.id,
                &ResourceSample::cpu_and_memory(at(1_000 + index), index as f32, 1_024 * index),
            )
            .expect("record sample");
    }

    let all = store.resource_samples(&service.id, 100).expect("all");
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].at, at(1_000));
    assert_eq!(all[4].memory_bytes, 4_096);

    let newest_two = store.resource_samples(&service.id, 2).expect("limited");
    let ats: Vec<_> = newest_two.iter().map(|s| s.at).collect();
    assert_eq!(ats, vec![at(1_003), at(1_004)]);
}

#[test]
fn resource_samples_round_trip_the_extra_fields() {
    let (store, _project, service) = seeded();
    let written = ResourceSample {
        at: at(2_000),
        cpu_percent: 8.0,
        memory_bytes: 4096,
        virtual_memory_bytes: 8192,
        thread_count: 11,
        disk_read_bytes: 128,
        disk_write_bytes: 64,
        connection_count: 3,
    };
    store
        .record_resource_sample(&service.id, &written)
        .expect("record");
    let read = store.resource_samples(&service.id, 1).expect("read");
    assert_eq!(read, vec![written]);
}

#[test]
fn retention_deletes_only_events_past_the_age_cutoff() {
    let (store, project, service) = seeded();
    let now = at(1_000_000);
    let old = 1_000_000 - 60_000; // 60s ago
    let fresh = 1_000_000 - 1_000; // 1s ago

    let events: Vec<_> = [old, old + 1, fresh, fresh + 1]
        .iter()
        .enumerate()
        .map(|(index, millis)| {
            event(
                u32::try_from(index).expect("small"),
                *millis,
                Some(&project.id),
                EventKind::ServiceStarted {
                    service_id: service.id.clone(),
                    pid: Some(1),
                },
            )
        })
        .collect();
    store.record_events(&events).expect("record");

    let policy = RetentionPolicy {
        max_events: usize::MAX,
        event_max_age: Duration::from_secs(30),
        ..RetentionPolicy::default()
    };
    let report = store.apply_retention(&policy, now).expect("retention");
    assert_eq!(report.events_deleted, 2);
    assert_eq!(report.samples_deleted, 0);

    let remaining: Vec<_> = store
        .recent_events(None, 100)
        .expect("events")
        .into_iter()
        .map(|e| e.at)
        .collect();
    assert_eq!(remaining, vec![at(fresh + 1), at(fresh)]);

    // A second pass has nothing left to do.
    let again = store.apply_retention(&policy, now).expect("second pass");
    assert_eq!(again.events_deleted, 0);
}

#[test]
fn retention_trims_the_oldest_events_over_the_row_cap() {
    let (store, project, service) = seeded();
    let events: Vec<_> = (0..1_500u32)
        .map(|index| {
            event(
                index,
                1_000 + u64::from(index),
                Some(&project.id),
                EventKind::ServiceStarted {
                    service_id: service.id.clone(),
                    pid: Some(1),
                },
            )
        })
        .collect();
    store.record_events(&events).expect("record");

    let policy = RetentionPolicy {
        max_events: 1_000,
        // Age must not participate: this test is about the row cap alone.
        event_max_age: Duration::from_secs(60 * 60 * 24 * 365),
        ..RetentionPolicy::default()
    };
    // The cap is larger than one delete batch, so this also proves the batching
    // loop finishes the job instead of stopping after 512 rows.
    let report = store
        .apply_retention(&policy, at(1_000_000))
        .expect("retention");
    assert_eq!(report.events_deleted, 500);

    let kept = store.recent_events(None, 10_000).expect("events");
    assert_eq!(kept.len(), 1_000);
    let oldest_kept = kept.last().expect("at least one").at;
    assert_eq!(oldest_kept, at(1_000 + 500), "oldest rows went first");
}

#[test]
fn retention_bounds_resource_samples_by_age_and_per_service_count() {
    let store = Store::open_in_memory().expect("open");
    let project = project("/tmp/shop", "shop");
    store.upsert_project(&project).expect("upsert project");
    let noisy = host_service(&project, "api", 3000);
    let quiet = host_service(&project, "worker", 4000);
    store.upsert_service(&noisy).expect("upsert noisy");
    store.upsert_service(&quiet).expect("upsert quiet");

    let now = at(1_000_000);
    // 20 recent samples for the noisy service, 2 of them ancient.
    for index in 0..20u64 {
        store
            .record_resource_sample(
                &noisy.id,
                &ResourceSample::cpu_and_memory(at(990_000 + index), 1.0, 1_024),
            )
            .expect("recent sample");
    }
    for index in 0..2u64 {
        store
            .record_resource_sample(
                &noisy.id,
                &ResourceSample::cpu_and_memory(at(1_000 + index), 1.0, 1_024),
            )
            .expect("ancient sample");
    }
    // The quiet service stays under the cap and must be left alone.
    for index in 0..3u64 {
        store
            .record_resource_sample(
                &quiet.id,
                &ResourceSample::cpu_and_memory(at(999_000 + index), 0.5, 512),
            )
            .expect("quiet sample");
    }

    let policy = RetentionPolicy {
        resource_sample_max_age: Duration::from_secs(60),
        max_resource_samples_per_service: 5,
        ..RetentionPolicy::default()
    };
    let report = store.apply_retention(&policy, now).expect("retention");
    // 2 aged out, then 15 of the remaining 20 trimmed to the cap.
    assert_eq!(report.samples_deleted, 17);
    assert_eq!(report.events_deleted, 0);

    let noisy_left = store.resource_samples(&noisy.id, 100).expect("noisy");
    assert_eq!(noisy_left.len(), 5);
    assert_eq!(noisy_left[0].at, at(990_015), "newest five survive");

    let quiet_left = store.resource_samples(&quiet.id, 100).expect("quiet");
    assert_eq!(quiet_left.len(), 3, "a service under the cap is untouched");
}

#[test]
fn retention_on_an_empty_database_reports_nothing() {
    let store = Store::open_in_memory().expect("open");
    let report = store
        .apply_retention(&RetentionPolicy::default(), at(1_000_000))
        .expect("retention");
    assert_eq!(report, Default::default());
}

#[test]
fn the_default_policy_keeps_a_days_events_and_an_hours_samples() {
    let policy = RetentionPolicy::default();
    assert_eq!(policy.event_max_age, Duration::from_secs(24 * 60 * 60));
    assert_eq!(policy.max_events, 50_000);
    assert_eq!(policy.resource_sample_max_age, Duration::from_secs(60 * 60));
    assert_eq!(policy.max_resource_samples_per_service, 3_600);
}
