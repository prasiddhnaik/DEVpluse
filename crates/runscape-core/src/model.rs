//! Core domain types (task T1.1).
//!
//! These are the shapes the whole system agrees on: collectors produce them,
//! the registry stores them, the API serialises them, the dashboard renders
//! them. They contain no platform types and no I/O.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::identity::{ContainerIdentity, Runtime};
use crate::ids::{ConnectionId, EventId, ProjectId, ServiceId};
use crate::project::{ProjectEvidence, RootKind};

/// How Runscape knows a relationship exists. The allowed set is fixed by
/// `AGENTS.md` rule 4; adding a member is a product decision, not a detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    /// Both ends of the socket were observed in the kernel's tables.
    ObservedSocket,
    /// Containers share a Docker network.
    DockerNetwork,
    /// Declared by the user or by a configuration file.
    Configured,
    /// Reported by an OpenTelemetry span.
    OtelSpan,
    /// Deduced. Must always carry an explanation and reduced confidence.
    Inferred,
}

impl EvidenceType {
    /// Baseline confidence for each evidence type (`ARCHITECTURE.md`).
    /// Inference must justify its own number, so it gets the floor.
    pub fn baseline_confidence(self) -> f32 {
        match self {
            Self::ObservedSocket | Self::OtelSpan => 1.00,
            Self::DockerNetwork => 0.80,
            Self::Configured => 0.90,
            Self::Inferred => 0.50,
        }
    }

    /// Whether the UI may present this as fact rather than as a suggestion.
    pub fn is_certain(self) -> bool {
        matches!(self, Self::ObservedSocket | Self::OtelSpan)
    }
}

/// Evidence attached to an edge or a membership. Every edge must have one
/// (`DECISIONS.md` D007).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub evidence_type: EvidenceType,
    /// `0.0..=1.0`.
    pub confidence: f32,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    /// Why, in words. Required for [`EvidenceType::Inferred`].
    pub detail: Option<String>,
}

impl Evidence {
    pub fn observed(evidence_type: EvidenceType, at: SystemTime) -> Self {
        Self {
            evidence_type,
            confidence: evidence_type.baseline_confidence(),
            first_seen: at,
            last_seen: at,
            detail: None,
        }
    }

    /// Inference always explains itself and never claims certainty.
    pub fn inferred(at: SystemTime, confidence: f32, detail: impl Into<String>) -> Self {
        Self {
            evidence_type: EvidenceType::Inferred,
            confidence: confidence.clamp(0.0, 0.95),
            first_seen: at,
            last_seen: at,
            detail: Some(detail.into()),
        }
    }

    /// Extend the observation window because the same fact was seen again.
    pub fn observe_again(&mut self, at: SystemTime) {
        if at > self.last_seen {
            self.last_seen = at;
        }
    }
}

/// A group of services the developer thinks of as one thing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub root: PathBuf,
    /// Directory name, or a user-supplied alias once aliases exist.
    pub name: String,
    pub kind: RootKind,
    /// Confidence of the root resolution that created this project.
    pub confidence: f32,
    pub evidence: Vec<ProjectEvidence>,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
}

impl Project {
    pub fn from_match(m: crate::project::ProjectMatch, at: SystemTime) -> Self {
        Self {
            id: ProjectId::derived(&m.root.to_string_lossy()),
            root: m.root,
            name: m.name,
            kind: m.kind,
            confidence: m.confidence,
            evidence: m
                .evidence
                .into_iter()
                // cwd depth belongs to a process, not to the project.
                .filter(|e| !matches!(e, ProjectEvidence::CwdAncestry { .. }))
                .collect(),
            first_seen: at,
            last_seen: at,
        }
    }
}

/// Whether a service runs as a host process or inside a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceKind {
    HostProcess,
    Container(ContainerIdentity),
}

/// Coarse health, derived from observation only. Runscape does not probe
/// application endpoints in the MVP beyond what the health engine is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Running and, if it listens, listening.
    Healthy,
    /// Running but something is off (restart loop, resource warning).
    Degraded,
    /// Known to Runscape but not currently running.
    Stopped,
    /// Not enough information.
    Unknown,
}

/// A logical service: the thing a developer names, independent of PID
/// (`DECISIONS.md` D006).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Service {
    pub id: ServiceId,
    pub project_id: Option<ProjectId>,
    /// Display name: compose service, executable name, or directory name.
    pub name: String,
    pub kind: ServiceKind,
    pub runtime: Runtime,
    /// The exact string the [`ServiceId`](crate::ids::ServiceId) was derived
    /// from, kept so identity decisions can be explained.
    pub fingerprint: String,
    pub health: Health,
    /// Currently running instances. Empty means stopped.
    pub instances: Vec<ProcessInstance>,
    /// Ports this service listens on.
    pub endpoints: Vec<Endpoint>,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    /// Restarts observed since the daemon started.
    pub restart_count: u32,
    /// Container CPU when Docker stats were sampled. `None` means not
    /// measured — never "zero idle". Host processes leave this empty and
    /// use [`ProcessInstance`] totals instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_cpu_percent: Option<f32>,
    /// See [`Service::measured_cpu_percent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_memory_bytes: Option<u64>,
}

impl Service {
    /// Whether the service is currently up.
    ///
    /// Host processes are live when they have at least one process instance.
    /// Containers never have instances — Docker's container list does not
    /// disclose the host PIDs of the processes inside a container — so their
    /// liveness is carried by [`Health`] instead. Keying container liveness off
    /// the instance count would report every container as stopped.
    pub fn is_running(&self) -> bool {
        match self.kind {
            ServiceKind::HostProcess => !self.instances.is_empty(),
            ServiceKind::Container(_) => {
                matches!(self.health, Health::Healthy | Health::Degraded)
            }
        }
    }

    /// Lowest listening port, used as the service's headline port.
    pub fn primary_port(&self) -> Option<u16> {
        self.endpoints.iter().map(|e| e.port).min()
    }

    pub fn total_memory_bytes(&self) -> u64 {
        if !self.instances.is_empty() {
            self.instances.iter().map(|i| i.memory_bytes).sum()
        } else {
            self.measured_memory_bytes.unwrap_or(0)
        }
    }

    pub fn total_cpu_percent(&self) -> f32 {
        if !self.instances.is_empty() {
            self.instances.iter().map(|i| i.cpu_percent).sum()
        } else {
            self.measured_cpu_percent.unwrap_or(0.0)
        }
    }

    pub fn total_virtual_memory_bytes(&self) -> u64 {
        self.instances.iter().map(|i| i.virtual_memory_bytes).sum()
    }

    pub fn total_thread_count(&self) -> u32 {
        self.thread_count().unwrap_or(0)
    }

    /// Sum of disclosed thread counts, or `None` when no instance reported one.
    pub fn thread_count(&self) -> Option<u32> {
        let mut total = 0u32;
        let mut any = false;
        for instance in &self.instances {
            if let Some(count) = instance.thread_count {
                total = total.saturating_add(count);
                any = true;
            }
        }
        any.then_some(total)
    }

    pub fn total_disk_read_bytes(&self) -> u64 {
        self.instances.iter().map(|i| i.disk_read_bytes).sum()
    }

    pub fn total_disk_write_bytes(&self) -> u64 {
        self.instances.iter().map(|i| i.disk_write_bytes).sum()
    }

    /// Host processes are measured when they have instances. Containers are
    /// measured only when Docker stats were collected (`--docker-stats`).
    /// Unmeasured is not the same as idle.
    pub fn resources_measured(&self) -> bool {
        match self.kind {
            ServiceKind::HostProcess => !self.instances.is_empty(),
            ServiceKind::Container(_) => {
                self.measured_cpu_percent.is_some() || self.measured_memory_bytes.is_some()
            }
        }
    }
}

/// One running process backing a service. Ephemeral by nature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub executable: Option<PathBuf>,
    /// Already redacted at capture time.
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub started_at_epoch_secs: u64,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// Virtual address space as the OS reported it. `0` on records written
    /// before this field existed.
    #[serde(default)]
    pub virtual_memory_bytes: u64,
    /// `None` when the OS did not disclose a thread count without enumerating
    /// Linux tasks as processes.
    #[serde(default)]
    pub thread_count: Option<u32>,
    /// Bytes read since the previous sample of this process, not a lifetime
    /// counter.
    #[serde(default)]
    pub disk_read_bytes: u64,
    /// Bytes written since the previous sample of this process.
    #[serde(default)]
    pub disk_write_bytes: u64,
}

/// A port a service listens on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub address: IpAddr,
    pub port: u16,
    pub protocol: Protocol,
    /// PID that owns the listening socket, when the OS disclosed it.
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

/// A directed edge between two services.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    /// The service that opened the connection.
    pub source: ServiceId,
    /// The service that accepted it.
    pub target: ServiceId,
    /// Port on the target that was connected to.
    pub target_port: u16,
    pub evidence: Evidence,
}

impl Connection {
    /// Edges are keyed on their endpoints so a reconnect updates the existing
    /// edge instead of accumulating duplicates.
    pub fn id_for(source: &ServiceId, target: &ServiceId, target_port: u16) -> ConnectionId {
        ConnectionId::derived(&format!("{source}->{target}:{target_port}"))
    }

    pub fn new(source: ServiceId, target: ServiceId, target_port: u16, evidence: Evidence) -> Self {
        Self {
            id: Self::id_for(&source, &target, target_port),
            source,
            target,
            target_port,
            evidence,
        }
    }
}

/// One resource measurement for a service, summed across its instances that tick.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResourceSample {
    pub at: SystemTime,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    #[serde(default)]
    pub virtual_memory_bytes: u64,
    #[serde(default)]
    pub thread_count: u32,
    /// Bytes read across instances since the previous sample of each process.
    #[serde(default)]
    pub disk_read_bytes: u64,
    /// Bytes written across instances since the previous sample of each process.
    #[serde(default)]
    pub disk_write_bytes: u64,
    /// Observed topology edges touching this service on this tick. Not network bytes.
    #[serde(default)]
    pub connection_count: u32,
}

impl ResourceSample {
    pub fn cpu_and_memory(at: SystemTime, cpu_percent: f32, memory_bytes: u64) -> Self {
        Self {
            at,
            cpu_percent,
            memory_bytes,
            virtual_memory_bytes: 0,
            thread_count: 0,
            disk_read_bytes: 0,
            disk_write_bytes: 0,
            connection_count: 0,
        }
    }

    /// Highest CPU and RSS in `history`, with the timestamps those peaks were
    /// observed. Empty history yields `None`.
    pub fn peaks(history: &[Self]) -> Option<ResourcePeaks> {
        let first = history.first()?;
        let mut peak_cpu = first.cpu_percent;
        let mut peak_cpu_at = first.at;
        let mut peak_memory = first.memory_bytes;
        let mut peak_memory_at = first.at;
        for sample in history.iter().skip(1) {
            if sample.cpu_percent > peak_cpu {
                peak_cpu = sample.cpu_percent;
                peak_cpu_at = sample.at;
            }
            if sample.memory_bytes > peak_memory {
                peak_memory = sample.memory_bytes;
                peak_memory_at = sample.at;
            }
        }
        Some(ResourcePeaks {
            cpu_percent: peak_cpu,
            cpu_at: peak_cpu_at,
            memory_bytes: peak_memory,
            memory_at: peak_memory_at,
        })
    }
}

/// Host-wide measurements taken with a process snapshot. Load averages are
/// what `sysinfo` reported; `process_count` is the size of that table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HostSample {
    pub at: SystemTime,
    #[serde(default)]
    pub load_avg_1: f64,
    #[serde(default)]
    pub load_avg_5: f64,
    #[serde(default)]
    pub load_avg_15: f64,
    #[serde(default)]
    pub process_count: u32,
}

/// Extremes of a retained [`ResourceSample`] window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResourcePeaks {
    pub cpu_percent: f32,
    pub cpu_at: SystemTime,
    pub memory_bytes: u64,
    pub memory_at: SystemTime,
}

/// Everything that can happen, as required by `SPEC.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    ProjectDetected {
        project_id: ProjectId,
    },
    ServiceStarted {
        service_id: ServiceId,
        /// `None` for a container: Docker's container list does not disclose
        /// the host PIDs of the processes inside it (`AGENTS.md` rule 3).
        pid: Option<u32>,
    },
    ServiceStopped {
        service_id: ServiceId,
        pid: Option<u32>,
    },
    ServiceRestarted {
        service_id: ServiceId,
        old_pid: Option<u32>,
        new_pid: Option<u32>,
    },
    PortOpened {
        service_id: Option<ServiceId>,
        port: u16,
    },
    PortClosed {
        service_id: Option<ServiceId>,
        port: u16,
    },
    ConnectionStarted {
        connection_id: ConnectionId,
        source: ServiceId,
        target: ServiceId,
        target_port: u16,
    },
    ConnectionEnded {
        connection_id: ConnectionId,
    },
    HealthChanged {
        service_id: ServiceId,
        from: Health,
        to: Health,
    },
    ResourceWarning {
        service_id: ServiceId,
        detail: String,
    },
    FileChanged {
        project_id: ProjectId,
        path: PathBuf,
    },
}

/// A recorded event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunscapeEvent {
    pub id: EventId,
    pub at: SystemTime,
    pub project_id: Option<ProjectId>,
    pub kind: EventKind,
}

/// Severity of a warning surfaced to the developer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A deterministic rule firing. Correlation, never causation
/// (`DECISIONS.md` D008).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable per (rule, subject) so a firing rule updates rather than spams.
    pub id: String,
    pub rule: String,
    pub severity: Severity,
    pub project_id: Option<ProjectId>,
    pub service_id: Option<ServiceId>,
    pub message: String,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    /// Events this warning was derived from.
    pub related_events: Vec<EventId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn observed_sockets_are_certain_and_inference_is_not() {
        assert!(EvidenceType::ObservedSocket.is_certain());
        assert!(!EvidenceType::Inferred.is_certain());
        assert_eq!(EvidenceType::ObservedSocket.baseline_confidence(), 1.00);
        assert_eq!(EvidenceType::DockerNetwork.baseline_confidence(), 0.80);
    }

    #[test]
    fn inference_can_never_claim_certainty() {
        let e = Evidence::inferred(t0(), 1.0, "same compose file");
        assert!(e.confidence < 1.0);
        assert!(e.detail.is_some());
    }

    #[test]
    fn repeated_observation_extends_the_window_only_forward() {
        let mut e = Evidence::observed(EvidenceType::ObservedSocket, t0());
        e.observe_again(t0() + Duration::from_secs(5));
        assert_eq!(e.first_seen, t0());
        assert_eq!(e.last_seen, t0() + Duration::from_secs(5));

        e.observe_again(t0());
        assert_eq!(e.last_seen, t0() + Duration::from_secs(5), "never rewinds");
    }

    #[test]
    fn connection_id_is_stable_for_the_same_endpoints() {
        let a = ServiceId::derived("a");
        let b = ServiceId::derived("b");
        assert_eq!(
            Connection::id_for(&a, &b, 5432),
            Connection::id_for(&a, &b, 5432)
        );
        assert_ne!(
            Connection::id_for(&a, &b, 5432),
            Connection::id_for(&b, &a, 5432),
            "direction matters"
        );
        assert_ne!(
            Connection::id_for(&a, &b, 5432),
            Connection::id_for(&a, &b, 6379)
        );
    }

    #[test]
    fn service_summarises_its_instances() {
        let endpoint = |port| Endpoint {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            protocol: Protocol::Tcp,
            pid: Some(1),
        };
        let instance = |pid, cpu, mem| ProcessInstance {
            pid,
            parent_pid: None,
            executable: None,
            command: vec![],
            cwd: None,
            started_at_epoch_secs: 1,
            cpu_percent: cpu,
            memory_bytes: mem,
            virtual_memory_bytes: mem * 2,
            thread_count: Some(4),
            disk_read_bytes: 0,
            disk_write_bytes: 0,
        };
        let service = Service {
            id: ServiceId::derived("x"),
            project_id: None,
            name: "web".into(),
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Node,
            fingerprint: "host|…".into(),
            health: Health::Healthy,
            instances: vec![instance(1, 10.0, 100), instance(2, 5.0, 50)],
            endpoints: vec![endpoint(8080), endpoint(3000)],
            first_seen: t0(),
            last_seen: t0(),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        };

        assert!(service.is_running());
        assert_eq!(service.primary_port(), Some(3000));
        assert_eq!(service.total_memory_bytes(), 150);
        assert_eq!(service.total_cpu_percent(), 15.0);
        assert_eq!(service.total_virtual_memory_bytes(), 300);
        assert_eq!(service.total_thread_count(), 8);
        assert!(service.resources_measured());
    }

    #[test]
    fn container_liveness_comes_from_health_not_instances() {
        let container = |health| Service {
            id: ServiceId::derived("c"),
            project_id: None,
            name: "postgres".into(),
            kind: ServiceKind::Container(crate::identity::ContainerIdentity {
                name: "app-postgres-1".into(),
                compose_project: Some("app".into()),
                compose_service: Some("postgres".into()),
            }),
            runtime: crate::identity::Runtime::Container,
            fingerprint: "container|app|postgres".into(),
            health,
            // Docker never discloses the host PIDs behind a container.
            instances: vec![],
            endpoints: vec![],
            first_seen: t0(),
            last_seen: t0(),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        };

        assert!(
            container(Health::Healthy).is_running(),
            "a running container must not look stopped just because it has no PIDs"
        );
        assert!(container(Health::Degraded).is_running());
        assert!(!container(Health::Stopped).is_running());
        assert!(!container(Health::Unknown).is_running());
    }

    #[test]
    fn a_host_process_with_no_instances_is_stopped() {
        let service = Service {
            id: ServiceId::derived("h"),
            project_id: None,
            name: "web".into(),
            kind: ServiceKind::HostProcess,
            runtime: crate::identity::Runtime::Node,
            fingerprint: "host|…".into(),
            // Health disagreeing with reality must not resurrect a host process.
            health: Health::Healthy,
            instances: vec![],
            endpoints: vec![],
            first_seen: t0(),
            last_seen: t0(),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        };
        assert!(!service.is_running());
        assert!(!service.resources_measured());
    }

    #[test]
    fn a_container_with_docker_stats_reports_measured_totals() {
        let mut postgres = container_service(Health::Healthy);
        postgres.measured_cpu_percent = Some(12.5);
        postgres.measured_memory_bytes = Some(64 * 1024 * 1024);
        assert_eq!(postgres.total_cpu_percent(), 12.5);
        assert_eq!(postgres.total_memory_bytes(), 64 * 1024 * 1024);
        assert!(postgres.resources_measured());
    }

    fn container_service(health: Health) -> Service {
        Service {
            id: ServiceId::derived("c"),
            project_id: None,
            name: "postgres".into(),
            kind: ServiceKind::Container(crate::identity::ContainerIdentity {
                name: "app-postgres-1".into(),
                compose_project: Some("app".into()),
                compose_service: Some("postgres".into()),
            }),
            runtime: crate::identity::Runtime::Container,
            fingerprint: "container|app|postgres".into(),
            health,
            instances: vec![],
            endpoints: vec![],
            first_seen: t0(),
            last_seen: t0(),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        }
    }

    #[test]
    fn old_resource_sample_json_fills_new_fields_with_zero() {
        let sample: ResourceSample = serde_json::from_str(
            r#"{"at":{"secs_since_epoch":1,"nanos_since_epoch":0},"cpu_percent":3.5,"memory_bytes":99}"#,
        )
        .expect("legacy sample");
        assert_eq!(sample.cpu_percent, 3.5);
        assert_eq!(sample.memory_bytes, 99);
        assert_eq!(sample.virtual_memory_bytes, 0);
        assert_eq!(sample.thread_count, 0);
        assert_eq!(sample.disk_read_bytes, 0);
        assert_eq!(sample.disk_write_bytes, 0);
        assert_eq!(sample.connection_count, 0);
    }

    #[test]
    fn peaks_are_the_maxima_in_the_window() {
        let history = [
            ResourceSample::cpu_and_memory(t0(), 1.0, 10),
            ResourceSample {
                at: t0() + std::time::Duration::from_secs(1),
                cpu_percent: 9.0,
                memory_bytes: 8,
                virtual_memory_bytes: 0,
                thread_count: 0,
                disk_read_bytes: 0,
                disk_write_bytes: 0,
                connection_count: 0,
            },
            ResourceSample::cpu_and_memory(t0() + std::time::Duration::from_secs(2), 2.0, 40),
        ];
        let peaks = ResourceSample::peaks(&history).expect("peaks");
        assert_eq!(peaks.cpu_percent, 9.0);
        assert_eq!(peaks.memory_bytes, 40);
    }
}
