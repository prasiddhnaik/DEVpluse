//! Daemon wiring (task T3.1).
//!
//! Binds loopback, runs the snapshot loop on a timer, and serves the API from
//! the state that loop produces.

use std::collections::BTreeSet;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use devpulse_core::ids::ProjectId;
use devpulse_discovery::watcher::ProjectWatcher;
use devpulse_docker::availability::DockerAvailability;
use devpulse_docker::collector::{BollardCollector, ContainerCollector};
use devpulse_events::EventDeriver;
use devpulse_events::warnings::{WarningEngine, WarningRules};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::api;
use crate::dto::DockerStatusDto;
use crate::persistence::{self, Persistence, RETENTION_INTERVAL, TickWrite};
use crate::security::{OriginPolicy, default_bind_addr, is_loopback_bind};
use crate::snapshot::{SnapshotConfig, SnapshotLoop};
use crate::state::{AppState, EVENT_RING_CAPACITY, TickUpdate};
use devpulse_storage::RetentionPolicy;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Must be loopback. Enforced at bind time, not trusted.
    pub bind: SocketAddr,
    pub snapshot: SnapshotConfig,
    pub origin_policy: OriginPolicy,
    /// Probing Docker costs one connect + ping at startup. Tests turn it off.
    pub probe_docker: bool,
    /// Sample per-container CPU/memory. Off by default: Docker's stats endpoint
    /// takes about a second to answer (it needs two samples), which would stall
    /// a 1 Hz loop (`BollardCollector::with_stats`).
    pub docker_stats: bool,
    /// Where history goes. `None` keeps history in memory only, so nothing
    /// survives a restart and nothing is written to the developer's disk.
    pub database: Option<PathBuf>,
    pub retention: RetentionPolicy,
    /// Watch the roots of projects that have running services, so a restart
    /// can be shown next to the save that preceded it (task T7.1).
    pub watch_files: bool,
    pub warning_rules: WarningRules,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind: default_bind_addr(),
            snapshot: SnapshotConfig::default(),
            origin_policy: OriginPolicy::default(),
            probe_docker: true,
            docker_stats: false,
            database: persistence::default_database_path(),
            retention: RetentionPolicy::default(),
            watch_files: true,
            warning_rules: WarningRules::default(),
        }
    }
}

/// A bound, not-yet-serving daemon. Binding first means the caller (and the
/// tests) know the real port before anything starts.
pub struct Daemon {
    listener: TcpListener,
    router: axum::Router,
    state: AppState,
    tick_interval: Duration,
    snapshot_config: SnapshotConfig,
    containers: Option<Box<dyn ContainerCollector>>,
    persistence: Option<Persistence>,
    watch_files: bool,
    warning_rules: WarningRules,
}

impl Daemon {
    pub async fn bind(config: DaemonConfig) -> anyhow::Result<Self> {
        if !is_loopback_bind(&config.bind) {
            // Refusing is the whole point: a daemon that can enumerate a
            // developer's processes must not be reachable from the network
            // (`AGENTS.md` rule 6).
            bail!(
                "refusing to bind {}: DevPulse serves loopback only",
                config.bind
            );
        }

        let (docker, containers) = if config.probe_docker {
            let availability = DockerAvailability::detect().await;
            let status = DockerStatusDto {
                available: availability.is_available(),
                reason: availability.reason().map(ToString::to_string),
            };
            let collector: Option<Box<dyn ContainerCollector>> = availability
                .into_collector()
                .map(|c: BollardCollector| Box::new(c.with_stats(config.docker_stats)) as _);
            (status, collector)
        } else {
            (
                DockerStatusDto {
                    available: false,
                    reason: Some("not probed".to_string()),
                },
                None,
            )
        };

        let listener = TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("binding {}", config.bind))?;

        let state = AppState::new(docker);

        // History is restored before the first tick so a restarted daemon shows
        // this morning's timeline instead of an empty one. Services are not
        // restored: those are rebuilt from observation (`AGENTS.md` rule 5).
        let persistence = match &config.database {
            None => None,
            Some(path) => {
                match persistence::restore(path, EVENT_RING_CAPACITY) {
                    Ok(history) => state.restore(history).await,
                    Err(error) => {
                        // A corrupt or unreadable database must not stop the
                        // daemon: observation is the product, history is not.
                        warn!(%error, path = %path.display(), "could not read stored history");
                    }
                }

                match Persistence::open(path, config.retention.clone()) {
                    Ok(persistence) => Some(persistence),
                    Err(error) => {
                        warn!(%error, path = %path.display(), "running without persistence");
                        None
                    }
                }
            }
        };

        let router = api::router(state.clone(), config.origin_policy);

        Ok(Self {
            listener,
            router,
            state,
            tick_interval: config.snapshot.tick_interval,
            snapshot_config: config.snapshot,
            containers,
            persistence,
            watch_files: config.watch_files,
            warning_rules: config.warning_rules,
        })
    }

    /// The address actually bound — the real port when the caller asked for 0.
    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        self.listener.local_addr().context("reading local address")
    }

    pub fn state(&self) -> AppState {
        self.state.clone()
    }

    /// Serve until `shutdown` resolves. The snapshot loop stops with the
    /// server: there is no point collecting for nobody.
    pub async fn serve_until(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let addr = self.local_addr()?;
        info!(%addr, "devpulse daemon listening");

        let collector = spawn_snapshot_loop(LoopParams {
            state: self.state.clone(),
            config: self.snapshot_config,
            interval: self.tick_interval,
            containers: self.containers,
            persistence: self.persistence,
            watch_files: self.watch_files,
            warning_rules: self.warning_rules,
        });

        let result = axum::serve(self.listener, self.router)
            .with_graceful_shutdown(shutdown)
            .await
            .context("serving http");

        collector.abort();
        result
    }

    /// Serve until Ctrl-C.
    pub async fn serve(self) -> anyhow::Result<()> {
        self.serve_until(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                error!(%error, "failed to listen for shutdown signal");
            }
        })
        .await
    }
}

/// Drive the snapshot loop on a fixed interval, folding each tick into the
/// shared state and broadcasting what changed.
/// Everything the loop task owns. Passed as one value because the task takes
/// ownership of all of it and a seven-argument function is not clearer.
struct LoopParams {
    state: AppState,
    config: SnapshotConfig,
    interval: Duration,
    containers: Option<Box<dyn ContainerCollector>>,
    persistence: Option<Persistence>,
    watch_files: bool,
    warning_rules: WarningRules,
}

fn spawn_snapshot_loop(params: LoopParams) -> JoinHandle<()> {
    let LoopParams {
        state,
        config,
        interval,
        containers,
        persistence,
        watch_files,
        warning_rules,
    } = params;

    tokio::spawn(async move {
        let mut snapshot = SnapshotLoop::new(config);
        if let Some(containers) = containers {
            snapshot = snapshot.with_containers(containers);
        }

        // A watcher that cannot start (no inotify slots left, an unsupported
        // filesystem) costs the correlation feature, not the daemon.
        let mut watcher = match watch_files.then(ProjectWatcher::new) {
            None => None,
            Some(Ok(watcher)) => Some(watcher),
            Some(Err(error)) => {
                warn!(%error, "file watching is unavailable; restarts will have no file context");
                None
            }
        };

        let mut warnings = WarningEngine::new(warning_rules);
        let mut deriver = EventDeriver::new();
        let mut ticker = tokio::time::interval(interval);
        // A tick that overruns must not cause a burst of catch-up ticks; the
        // next one is simply late (`AGENTS.md` rule 7).
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_retention = SystemTime::now();
        let mut idle_skips = 0u32;
        let mut persist_tick = 0u32;
        let mut persisted_projects: BTreeSet<ProjectId> = BTreeSet::new();

        loop {
            ticker.tick().await;

            // No dashboard attached: keep observing, just not at 1 Hz. Tests
            // use a sub-second interval and must not be stretched.
            if interval >= Duration::from_secs(1) && state.ws_subscribers() == 0 {
                idle_skips += 1;
                if idle_skips < IDLE_TICKS {
                    continue;
                }
            }
            idle_skips = 0;

            let tick = match snapshot.tick().await {
                Ok(tick) => tick,
                Err(error) => {
                    // A failed collection is not fatal: the machine may be
                    // busy or a permission may have changed. Keep the last
                    // known world and try again next tick.
                    error!(%error, "snapshot tick failed");
                    continue;
                }
            };

            let at = tick.at.unwrap_or_else(std::time::SystemTime::now);
            let mut events = deriver.derive(&tick.registry_delta, &tick.topology_delta, at);
            for project in snapshot.projects().keys() {
                if let Some(event) = deriver.announce_project(project, at) {
                    events.push(event);
                }
            }

            if let Some(watcher) = &mut watcher {
                // Watch exactly the projects that currently have something
                // running: a project nobody is running is not being edited in a
                // way DevPulse can correlate with anything.
                let roots: Vec<PathBuf> = snapshot
                    .projects()
                    .values()
                    .filter(|project| {
                        snapshot
                            .registry()
                            .services()
                            .any(|s| s.project_id.as_ref() == Some(&project.id) && s.is_running())
                    })
                    .map(|project| project.root.clone())
                    .collect();
                watcher.sync_roots(&roots);

                for change in watcher.drain(MAX_FILE_EVENTS_PER_TICK) {
                    let Some(project) = snapshot
                        .projects()
                        .values()
                        .find(|project| project.root == change.root)
                    else {
                        continue;
                    };
                    events.push(deriver.file_changed(&project.id, change.path, change.at));
                }
            }

            let services: Vec<_> = snapshot.registry().services().cloned().collect();
            let connections = snapshot.topology().connections().to_vec();
            let projects = snapshot.projects().clone();

            let changed = changed_service_ids(&tick.registry_delta);
            let changed_services: Vec<_> = services
                .iter()
                .filter(|s| changed.contains(&s.id))
                .cloned()
                .collect();
            let stored_events = events.clone();

            let applied = state
                .apply_tick(TickUpdate {
                    tick: &tick,
                    projects: &projects,
                    services,
                    connections,
                    events,
                    warnings: Some(&mut warnings),
                })
                .await;

            if let Some(persistence) = &persistence {
                persist_tick = persist_tick.wrapping_add(1);
                let new_projects: Vec<_> = projects
                    .values()
                    .filter(|project| persisted_projects.insert(project.id.clone()))
                    .cloned()
                    .collect();
                // In-memory sparklines stay at 1 Hz; SQLite does not need a
                // row per service per second.
                let samples = if persist_tick % SAMPLE_PERSIST_EVERY == 1 {
                    tick.samples.clone()
                } else {
                    Vec::new()
                };
                persistence.write(TickWrite {
                    projects: new_projects,
                    services: changed_services,
                    events: stored_events,
                    samples,
                    warnings: if applied.warnings_changed {
                        applied.warnings.clone()
                    } else {
                        Vec::new()
                    },
                });

                if at
                    .duration_since(last_retention)
                    .is_ok_and(|since| since >= RETENTION_INTERVAL)
                {
                    persistence.retention(at);
                    last_retention = at;
                }
            }

            debug!(
                frames = applied.frames.len(),
                warnings = applied.warnings.len(),
                duration_ms = tick.collector_duration_ms,
                "tick applied"
            );

            for frame in applied.frames {
                state.publish(frame);
            }
        }
    })
}

/// Cap on file events folded into a single tick. A `git checkout` of a large
/// branch must not turn into thousands of events; the developer needs to know
/// files changed, not which two thousand.
const MAX_FILE_EVENTS_PER_TICK: usize = 20;

/// When nobody is listening on the WebSocket, collect once every N wakeups
/// instead of every interval. Opening the dashboard resumes full rate within
/// one interval.
const IDLE_TICKS: u32 = 5;

/// Persist resource samples every N ticks. The in-memory ring is still 1 Hz.
const SAMPLE_PERSIST_EVERY: u32 = 5;

/// Ids the registry reported as changed this tick, in one sorted set.
fn changed_service_ids(
    delta: &devpulse_core::registry::RegistryDelta,
) -> std::collections::BTreeSet<devpulse_core::ids::ServiceId> {
    delta
        .started
        .iter()
        .map(|(id, _)| id.clone())
        .chain(delta.stopped.iter().map(|(id, _)| id.clone()))
        .chain(delta.restarted.iter().map(|(id, _, _)| id.clone()))
        .chain(delta.ports_opened.iter().map(|(id, _)| id.clone()))
        .chain(delta.ports_closed.iter().map(|(id, _)| id.clone()))
        .chain(delta.health_changed.iter().map(|(id, _, _)| id.clone()))
        .collect()
}
