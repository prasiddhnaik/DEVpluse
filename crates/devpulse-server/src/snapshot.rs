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

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime};

use devpulse_core::grouping::{GroupingEngine, GroupingInput};
use devpulse_core::identity::{Runtime, ServiceFingerprint};
use devpulse_core::ids::{ProjectId, ServiceId};
use devpulse_core::model::{Endpoint, ProcessInstance, Protocol, Service, ServiceKind};
use devpulse_core::model::{Project, ResourceSample};
use devpulse_core::project::{ProjectResolver, ResolverConfig};
use devpulse_core::registry::{RegistryDelta, ServiceObservation, ServiceRegistry};
use devpulse_core::service_filter::{is_build_tool, is_bundled_app, is_system_tool};
use devpulse_core::topology::{ObservedConnectionEndpoints, TopologyBuilder, TopologyDelta};
use devpulse_discovery::error::CollectorError;
use devpulse_discovery::process::{ObservedProcess, ProcessCollector, SysinfoProcessCollector};
use devpulse_discovery::socket::{Netstat2SocketCollector, SocketCollector};
use devpulse_docker::collector::ContainerCollector;
use tracing::{debug, warn};

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
    /// Container collector only: why the last collection produced nothing.
    /// `None` means it produced something (or was never asked).
    pub error: Option<String>,
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
    /// `None` when the daemon has no Docker collector at all.
    pub container: Option<CollectorTiming>,
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
    /// Present only when a Docker daemon answered a ping at startup
    /// (`DockerAvailability::detect`). Absent is a supported, normal state.
    container_collector: Option<Box<dyn ContainerCollector>>,
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
            socket_collector: Netstat2SocketCollector::tcp_only(),
            _project_resolver: project_resolver,
            grouping_engine,
            container_collector: None,
            registry: ServiceRegistry::new(),
            topology_builder: TopologyBuilder::new(),
            projects: BTreeMap::new(),
        }
    }

    /// Add Docker inspection. Without this the loop reports host processes
    /// only, which is what happens on a machine with no Docker.
    pub fn with_containers(mut self, collector: Box<dyn ContainerCollector>) -> Self {
        self.container_collector = Some(collector);
        self
    }

    pub fn inspects_containers(&self) -> bool {
        self.container_collector.is_some()
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
        // Docker runs alongside the OS collectors rather than after them: it is
        // an HTTP round trip and there is no reason to pay for it serially.
        let container_future = async {
            match &self.container_collector {
                None => None,
                Some(collector) => Some(collector.snapshot().await),
            }
        };
        let (process_snapshot, socket_snapshot, container_result) = tokio::join!(
            self.process_collector.snapshot(),
            self.socket_collector.snapshot(),
            container_future,
        );

        let process_snapshot = process_snapshot.map_err(|e| SnapshotError::CollectionFailed {
            what: "process".to_string(),
            source: e,
        })?;

        let socket_snapshot = socket_snapshot.map_err(|e| SnapshotError::CollectionFailed {
            what: "socket".to_string(),
            source: e,
        })?;

        // Docker failing is not the daemon failing: a daemon that was stopped
        // between ticks must degrade to host processes, not stop the loop
        // (`AGENTS.md` rule 3).
        let (container_snapshot, container_error) = match container_result {
            None => (None, None),
            Some(Ok(snapshot)) => (Some(snapshot), None),
            Some(Err(error)) => {
                warn!(%error, "container collection failed; continuing without containers");
                (None, Some(error.to_string()))
            }
        };

        let collector_duration_ms = start.elapsed().as_millis() as u64;

        debug!(
            duration_ms = collector_duration_ms,
            processes = process_snapshot.processes.len(),
            sockets = socket_snapshot.sockets.len(),
            containers = container_snapshot
                .as_ref()
                .map(|s| s.containers.len())
                .unwrap_or_default(),
            "snapshot collected"
        );

        // 2. Group processes into projects. Shells, compilers and other
        // people's processes are not services; walking their cwd trees for
        // `.git` is the expensive half of a tick, so they never enter grouping.
        let listening_pids: HashSet<u32> = socket_snapshot
            .sockets
            .iter()
            .filter(|s| s.is_listening())
            .flat_map(|s| s.pids.iter().copied())
            .collect();

        let mut group_pids: HashSet<u32> = HashSet::new();
        for process in &process_snapshot.processes {
            if !worth_grouping(process, listening_pids.contains(&process.pid)) {
                continue;
            }
            group_pids.insert(process.pid);
            if let Some(parent) = process.parent_pid {
                // A listener with no readable cwd inherits its parent's project.
                group_pids.insert(parent);
            }
        }

        let grouping_inputs: Vec<GroupingInput> = process_snapshot
            .processes
            .iter()
            .filter(|p| group_pids.contains(&p.pid))
            .map(|p| GroupingInput {
                pid: p.pid,
                parent_pid: p.parent_pid,
                cwd: p.cwd.clone(),
                container: None,
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

            // A shell, a `sleep`, or the `git` a script just ran is a process
            // in a project directory, not a service. Left in, a short-lived
            // system tool that runs repeatedly reads as a service in a restart
            // loop (`service_filter`).
            if !devpulse_core::service_filter::is_service_process(
                process.executable.as_deref(),
                !endpoints.is_empty(),
                Some(Duration::from_secs(process.run_time_secs)),
            ) {
                continue;
            }

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
                instance: Some(ProcessInstance {
                    pid: process.pid,
                    parent_pid: process.parent_pid,
                    executable: process.executable.clone(),
                    command: process.command.clone(),
                    cwd: process.cwd.clone(),
                    started_at_epoch_secs: process.start_time_epoch_secs,
                    cpu_percent: process.cpu_percent,
                    memory_bytes: process.memory_bytes,
                }),
                endpoints,
            });
        }

        // 3b. Group and convert containers. Grouping runs as its own batch:
        // a container has no PID, so the `pid` field is only a row key here,
        // and mixing the batches would let a container collide with a process.
        if let Some(containers) = &container_snapshot {
            let running: Vec<_> = containers.running().collect();
            let container_inputs: Vec<GroupingInput> = running
                .iter()
                .enumerate()
                .map(|(row, container)| GroupingInput {
                    pid: row as u32,
                    parent_pid: None,
                    cwd: None,
                    container: Some(container.identity.clone()),
                })
                .collect();

            let container_grouping = self.grouping_engine.group(&container_inputs, at);
            self.remember_projects(&container_grouping.projects);

            for (row, container) in running.iter().enumerate() {
                let project_id = container_grouping
                    .membership_for(row as u32)
                    .map(|m| m.project_id.clone());
                observations.push(container.to_observation(project_id));
            }
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
                error: None,
            },
            socket: CollectorTiming {
                duration_ms: socket_snapshot.duration.as_millis() as u64,
                last_run: Some(socket_snapshot.captured_at),
                degraded_fields: BTreeMap::new(),
                sockets_without_owner: Some(socket_snapshot.sockets_without_owner),
                error: None,
            },
            container: self.container_collector.as_ref().map(|_| CollectorTiming {
                duration_ms: container_snapshot
                    .as_ref()
                    .map(|s| s.duration.as_millis() as u64)
                    .unwrap_or_default(),
                last_run: container_snapshot.as_ref().map(|s| s.captured_at),
                degraded_fields: BTreeMap::new(),
                sockets_without_owner: None,
                error: container_error,
            }),
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

/// Whether a process is worth walking a filesystem tree for.
///
/// Listeners always are: a port is a service. Everything else must look like
/// developer software rather than a shell, a compiler, or a process the OS
/// would not identify. Parent PIDs of those candidates are added separately so
/// a listener with no cwd can still inherit a project.
fn worth_grouping(process: &ObservedProcess, listening: bool) -> bool {
    if listening {
        return true;
    }
    let Some(cwd) = process.cwd.as_deref() else {
        return false;
    };
    if is_bundled_app(cwd) {
        return false;
    }
    match process.executable.as_deref() {
        None => false,
        Some(exe) if is_system_tool(exe) || is_build_tool(exe) || is_bundled_app(exe) => false,
        Some(_) => true,
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

    #[test]
    fn shells_and_compilers_are_not_grouped() {
        let shell = ObservedProcess {
            pid: 1,
            parent_pid: Some(0),
            name: "zsh".into(),
            executable: Some("/bin/zsh".into()),
            command: vec!["zsh".into()],
            cwd: Some("/Users/dev/app".into()),
            cpu_percent: 0.0,
            memory_bytes: 0,
            virtual_memory_bytes: 0,
            start_time_epoch_secs: 1,
            run_time_secs: 600,
            state: devpulse_discovery::process::ProcessState::Sleeping,
            user_id: None,
        };
        assert!(!worth_grouping(&shell, false));
        assert!(
            worth_grouping(&shell, true),
            "a listening shell is a service"
        );
    }

    fn observed(exe: &str, cwd: &str) -> ObservedProcess {
        ObservedProcess {
            pid: 10,
            parent_pid: Some(1),
            name: "app".into(),
            executable: Some(exe.into()),
            command: vec!["app".into()],
            cwd: Some(cwd.into()),
            cpu_percent: 0.0,
            memory_bytes: 0,
            virtual_memory_bytes: 0,
            start_time_epoch_secs: 1,
            run_time_secs: 600,
            state: devpulse_discovery::process::ProcessState::Sleeping,
            user_id: None,
        }
    }

    #[test]
    fn bundled_apps_are_not_grouped_unless_listening() {
        let chrome = observed(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Users/dev",
        );
        assert!(!worth_grouping(&chrome, false));
        assert!(
            worth_grouping(&chrome, true),
            "a listening app bundle is still a service"
        );

        let helper_cwd = observed(
            "/Users/dev/project/target/debug/worker",
            "/Applications/Some.app/Contents/Resources",
        );
        assert!(!worth_grouping(&helper_cwd, false));
        assert!(worth_grouping(&helper_cwd, true));

        let project_worker = observed(
            "/Users/dev/project/target/debug/worker",
            "/Users/dev/project",
        );
        assert!(worth_grouping(&project_worker, false));
    }
}
