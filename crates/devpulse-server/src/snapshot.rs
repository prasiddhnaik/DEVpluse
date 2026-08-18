//! Snapshot loop (task T2.1).
//!
//! Runs process and socket collectors continuously, converts observations into
//! service registry updates, builds topology, and derives events.
//!
//! The loop runs at a fixed interval (1 second by default, per `TASKS.md`) and
//! coordinates:
//! - process/socket collection (blocking, on spawn_blocking)
//! - project grouping
//! - service registry reconciliation
//! - topology construction
//! - event derivation
//! - resource sampling

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use devpulse_core::grouping::{GroupingEngine, GroupingInput};
use devpulse_core::identity::{Runtime, ServiceFingerprint};
use devpulse_core::ids::{ProjectId, ServiceId};
use devpulse_core::model::{Endpoint, ProcessInstance, Protocol, Service, ServiceKind};
use devpulse_core::model::{Project, ResourceSample};
use devpulse_core::project::{ProjectResolver, ResolverConfig};
use devpulse_core::registry::{RegistryDelta, ServiceObservation, ServiceRegistry};
use devpulse_core::topology::{ObservedConnectionEndpoints, TopologyBuilder, TopologyDelta};
use devpulse_discovery::error::CollectorError;
use devpulse_discovery::process::{ProcessCollector, SysinfoProcessCollector};
use devpulse_discovery::socket::{Netstat2SocketCollector, SocketCollector};
use tracing::debug;

/// Default polling interval: 1 second (`TASKS.md`).
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Snapshot loop configuration.
#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// How often to collect and reconcile.
    pub tick_interval: Duration,
    /// Project resolver configuration.
    pub resolver_config: ResolverConfig,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            tick_interval: DEFAULT_TICK_INTERVAL,
            resolver_config: ResolverConfig::default(),
        }
    }
}

/// How one collector behaved on the last tick. Surfaced by `/api/v1/status`
/// so a developer can see *why* the view is thin — a degraded field count is
/// the honest version of missing data (`AGENTS.md` rule 3).
#[derive(Debug, Clone, Default)]
pub struct CollectorTiming {
    pub duration_ms: u64,
    pub last_run: Option<SystemTime>,
    /// Field name -> number of processes it could not be read for.
    pub degraded_fields: BTreeMap<String, usize>,
    /// Socket collector only: sockets the OS would not attribute to a PID.
    pub sockets_without_owner: Option<usize>,
}

/// Result of one snapshot tick.
#[derive(Debug, Default)]
pub struct TickResult {
    /// The instant the tick reconciled against; every derived event uses it.
    pub at: Option<SystemTime>,
    pub registry_delta: RegistryDelta,
    pub topology_delta: TopologyDelta,
    /// One sample per running service, for whoever keeps history.
    pub samples: Vec<(ServiceId, ResourceSample)>,
    /// Wall time of the whole collection phase (both collectors run together).
    pub collector_duration_ms: u64,
    pub process: CollectorTiming,
    pub socket: CollectorTiming,
}

/// The snapshot loop engine.
///
/// Holds all the state needed to run continuous discovery and reconciliation.
pub struct SnapshotLoop {
    _config: SnapshotConfig,
    process_collector: SysinfoProcessCollector,
    socket_collector: Netstat2SocketCollector,
    _project_resolver: ProjectResolver,
    grouping_engine: GroupingEngine,
    registry: ServiceRegistry,
    topology_builder: TopologyBuilder,
    /// Projects seen on the most recent tick. The grouping engine is
    /// stateless per tick, so the loop is what remembers them.
    projects: BTreeMap<ProjectId, Project>,
}

impl SnapshotLoop {
    pub fn new(config: SnapshotConfig) -> Self {
        let project_resolver = ProjectResolver::new(config.resolver_config.clone());
        let grouping_engine = GroupingEngine::new(project_resolver.clone());

        Self {
            _config: config,
            process_collector: SysinfoProcessCollector::new(),
            socket_collector: Netstat2SocketCollector::new(),
            _project_resolver: project_resolver,
            grouping_engine,
            registry: ServiceRegistry::new(),
            topology_builder: TopologyBuilder::new(),
            projects: BTreeMap::new(),
        }
    }

    /// Run one tick of the snapshot loop.
    ///
    /// This is the core reconciliation logic: collect, group, reconcile, build
    /// topology, derive events. Returns what changed so the caller can emit
    /// events and broadcast updates.
    pub async fn tick(&mut self) -> Result<TickResult, SnapshotError> {
        let at = SystemTime::now();
        let start = std::time::Instant::now();

        // 1. Collect (async collectors handle their own spawn_blocking).
        let (process_snapshot, socket_snapshot) = tokio::join!(
            self.process_collector.snapshot(),
            self.socket_collector.snapshot(),
        );

        let process_snapshot = process_snapshot.map_err(|e| SnapshotError::CollectionFailed {
            what: "process".to_string(),
            source: e,
        })?;

        let socket_snapshot = socket_snapshot.map_err(|e| SnapshotError::CollectionFailed {
            what: "socket".to_string(),
            source: e,
        })?;

        let collector_duration_ms = start.elapsed().as_millis() as u64;

        debug!(
            duration_ms = collector_duration_ms,
            processes = process_snapshot.processes.len(),
            sockets = socket_snapshot.sockets.len(),
            "snapshot collected"
        );

        // 2. Group processes into projects.
        let grouping_inputs: Vec<GroupingInput> = process_snapshot
            .processes
            .iter()
            .map(|p| GroupingInput {
                pid: p.pid,
                parent_pid: p.parent_pid,
                cwd: p.cwd.clone(),
                container: None, // TODO: add Docker when Milestone 6 is done
            })
            .collect();

        let grouping_outcome = self.grouping_engine.group(&grouping_inputs, at);
        self.remember_projects(&grouping_outcome.projects);

        // 3. Convert processes to service observations.
        let mut observations = Vec::new();
        for process in &process_snapshot.processes {
            let Some(membership) = grouping_outcome.membership_for(process.pid) else {
                continue; // Ungrouped processes are not services.
            };

            let project_id = Some(membership.project_id.clone());
            let runtime = Runtime::from_executable(process.executable.as_deref());
            let _fingerprint = ServiceFingerprint::host(
                project_id.as_ref(),
                runtime,
                process.executable.as_deref(),
                process.cwd.as_deref(),
                None, // Primary port determined from endpoints below
            );

            // Find listening ports for this process.
            let endpoints: Vec<Endpoint> = socket_snapshot
                .sockets
                .iter()
                .filter(|s| s.is_listening() && s.owned_by(process.pid))
                .map(|s| Endpoint {
                    address: s.local_addr,
                    port: s.local_port,
                    protocol: match s.protocol {
                        devpulse_discovery::socket::Protocol::Tcp => Protocol::Tcp,
                        devpulse_discovery::socket::Protocol::Udp => Protocol::Udp,
                    },
                    pid: s.pids.first().copied(),
                })
                .collect();

            let primary_port = endpoints.iter().map(|e| e.port).min();

            // Rebuild fingerprint with the primary port.
            let fingerprint = ServiceFingerprint::host(
                project_id.as_ref(),
                runtime,
                process.executable.as_deref(),
                process.cwd.as_deref(),
                primary_port,
            );

            let name = process
                .executable
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or(&process.name)
                .to_string();

            observations.push(ServiceObservation {
                fingerprint,
                name,
                project_id,
                kind: ServiceKind::HostProcess,
                runtime,
                instance: ProcessInstance {
                    pid: process.pid,
                    parent_pid: process.parent_pid,
                    executable: process.executable.clone(),
                    command: process.command.clone(),
                    cwd: process.cwd.clone(),
                    started_at_epoch_secs: process.start_time_epoch_secs,
                    cpu_percent: process.cpu_percent,
                    memory_bytes: process.memory_bytes,
                },
                endpoints,
            });
        }

        // 4. Reconcile with the registry.
        let registry_delta = self.registry.apply(observations, at);

        // 5. Build topology from socket connections.
        let established: Vec<ObservedConnectionEndpoints> = socket_snapshot
            .sockets
            .iter()
            .filter(|s| s.is_established())
            .filter_map(|s| {
                // Only use sockets with a single owning PID for topology
                let pid = s.pids.first().copied()?;
                let remote_addr = s.remote_addr?;
                let remote_port = s.remote_port?;
                Some(ObservedConnectionEndpoints {
                    local_addr: s.local_addr,
                    local_port: s.local_port,
                    remote_addr,
                    remote_port,
                    pid: Some(pid),
                })
            })
            .collect();

        let services: Vec<&Service> = self.registry.services().collect();
        let topology_delta = self.topology_builder.observe(&services, &established, at);

        // 6. Sample resources. The samples are returned rather than stored:
        // history belongs to whoever serves it (`state::RuntimeView`), so
        // there is exactly one copy of it in the daemon.
        let samples: Vec<(ServiceId, ResourceSample)> = services
            .iter()
            .filter(|s| s.is_running())
            .map(|s| {
                (
                    s.id.clone(),
                    ResourceSample {
                        at,
                        cpu_percent: s.total_cpu_percent(),
                        memory_bytes: s.total_memory_bytes(),
                    },
                )
            })
            .collect();

        Ok(TickResult {
            at: Some(at),
            registry_delta,
            topology_delta,
            samples,
            collector_duration_ms,
            process: CollectorTiming {
                duration_ms: process_snapshot.duration.as_millis() as u64,
                last_run: Some(process_snapshot.captured_at),
                degraded_fields: degraded_fields(&process_snapshot.degradations),
                sockets_without_owner: None,
            },
            socket: CollectorTiming {
                duration_ms: socket_snapshot.duration.as_millis() as u64,
                last_run: Some(socket_snapshot.captured_at),
                degraded_fields: BTreeMap::new(),
                sockets_without_owner: Some(socket_snapshot.sockets_without_owner),
            },
        })
    }

    /// Merge this tick's projects into the remembered set, preserving
    /// `first_seen`: a project that has been up all morning must not look new
    /// because the grouping engine rebuilt it a second ago.
    fn remember_projects(&mut self, seen: &BTreeMap<ProjectId, Project>) {
        for (id, project) in seen {
            match self.projects.get_mut(id) {
                Some(existing) => {
                    existing.last_seen = project.last_seen;
                    existing.name = project.name.clone();
                    existing.confidence = project.confidence;
                }
                None => {
                    self.projects.insert(id.clone(), project.clone());
                }
            }
        }
        // A project with no live service is gone; the registry is the authority
        // on what is still running.
        let live: std::collections::BTreeSet<ProjectId> = self
            .registry
            .services()
            .filter_map(|s| s.project_id.clone())
            .collect();
        self.projects
            .retain(|id, _| seen.contains_key(id) || live.contains(id));
    }

    /// Projects observed on the most recent tick.
    pub fn projects(&self) -> &BTreeMap<ProjectId, Project> {
        &self.projects
    }

    /// Accessors for the current state.
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }

    pub fn topology(&self) -> &devpulse_core::topology::Topology {
        self.topology_builder.topology()
    }

    pub fn grouping_engine(&self) -> &GroupingEngine {
        &self.grouping_engine
    }
}

/// Name the missing fields the way `/status` reports them. Only non-zero
/// counts are reported: an empty map means nothing was degraded.
fn degraded_fields(d: &devpulse_discovery::process::Degradations) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (name, count) in [
        ("cwd", d.missing_cwd),
        ("executable", d.missing_executable),
        ("command", d.missing_command),
        ("parent_pid", d.missing_parent),
        ("user", d.missing_user),
    ] {
        if count > 0 {
            out.insert(name.to_string(), count);
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{what} collection failed: {source}")]
    CollectionFailed {
        what: String,
        #[source]
        source: CollectorError,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_loop_constructs() {
        let config = SnapshotConfig::default();
        let loop_ = SnapshotLoop::new(config);
        assert!(loop_.registry().is_empty());
    }

    #[test]
    fn default_config_is_one_second() {
        let config = SnapshotConfig::default();
        assert_eq!(config.tick_interval, DEFAULT_TICK_INTERVAL);
    }

    #[tokio::test]
    async fn tick_collects_and_reconciles() {
        let mut loop_ = SnapshotLoop::new(SnapshotConfig::default());
        let result = loop_.tick().await.expect("tick succeeds");

        // Should complete without error
        assert!(result.collector_duration_ms > 0);
        // Registry may be empty if no processes are grouped, but should not panic
        let _ = loop_.registry().services().count();
    }
}
