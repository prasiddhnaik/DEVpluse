//! Fixtures shared by the daemon's integration tests.
//!
//! State is seeded rather than collected: the API contract is what these tests
//! are about, not what happens to be running on the machine.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devpulse_core::identity::Runtime;
use devpulse_core::ids::{ProjectId, ServiceId};
use devpulse_core::model::{
    Endpoint, Health, ProcessInstance, Project, Protocol, Service, ServiceKind,
};
use devpulse_core::project::RootKind;
use devpulse_core::registry::RegistryDelta;
use devpulse_core::topology::TopologyDelta;
use devpulse_server::snapshot::TickResult;

pub fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
}

pub fn project() -> Project {
    Project {
        id: ProjectId::derived("/tmp/devpulse-fixture"),
        root: "/tmp/devpulse-fixture".into(),
        name: "devpulse-fixture".to_string(),
        kind: RootKind::GitRepository,
        confidence: 0.95,
        evidence: Vec::new(),
        first_seen: at(0),
        last_seen: at(10),
    }
}

pub fn service(name: &str, port: u16, pid: u32) -> Service {
    let project = project();
    Service {
        id: ServiceId::derived(&format!("host|{}|{name}|{port}", project.id)),
        project_id: Some(project.id),
        name: name.to_string(),
        kind: ServiceKind::HostProcess,
        runtime: Runtime::Node,
        fingerprint: format!("host|{name}|{port}"),
        health: Health::Healthy,
        instances: vec![ProcessInstance {
            pid,
            parent_pid: Some(1),
            executable: Some("/usr/local/bin/node".into()),
            command: vec!["node".to_string(), "server.js".to_string()],
            cwd: Some("/tmp/devpulse-fixture".into()),
            started_at_epoch_secs: 1_700_000_000,
            cpu_percent: 1.5,
            memory_bytes: 40 * 1024 * 1024,
        }],
        endpoints: vec![Endpoint {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            protocol: Protocol::Tcp,
            pid: Some(pid),
        }],
        first_seen: at(0),
        last_seen: at(10),
        restart_count: 0,
    }
}

pub fn tick(delta: RegistryDelta, topology: TopologyDelta) -> TickResult {
    TickResult {
        at: Some(at(10)),
        registry_delta: delta,
        topology_delta: topology,
        samples: Vec::new(),
        collector_duration_ms: 4,
        process: Default::default(),
        socket: Default::default(),
        container: None,
    }
}
