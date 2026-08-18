//! Daemon wiring (task T3.1).
//!
//! Binds loopback, runs the snapshot loop on a timer, and serves the API from
//! the state that loop produces.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use devpulse_docker::availability::DockerAvailability;
use devpulse_events::EventDeriver;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::api;
use crate::dto::DockerStatusDto;
use crate::security::{OriginPolicy, default_bind_addr, is_loopback_bind};
use crate::snapshot::{SnapshotConfig, SnapshotLoop};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Must be loopback. Enforced at bind time, not trusted.
    pub bind: SocketAddr,
    pub snapshot: SnapshotConfig,
    pub origin_policy: OriginPolicy,
    /// Probing Docker costs one connect + ping at startup. Tests turn it off.
    pub probe_docker: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind: default_bind_addr(),
            snapshot: SnapshotConfig::default(),
            origin_policy: OriginPolicy::default(),
            probe_docker: true,
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

        let docker = if config.probe_docker {
            let availability = DockerAvailability::detect().await;
            DockerStatusDto {
                available: availability.is_available(),
                reason: availability.reason().map(ToString::to_string),
            }
        } else {
            DockerStatusDto {
                available: false,
                reason: Some("not probed".to_string()),
            }
        };

        let listener = TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("binding {}", config.bind))?;

        let state = AppState::new(docker);
        let router = api::router(state.clone(), config.origin_policy);

        Ok(Self {
            listener,
            router,
            state,
            tick_interval: config.snapshot.tick_interval,
            snapshot_config: config.snapshot,
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

        let collector =
            spawn_snapshot_loop(self.state.clone(), self.snapshot_config, self.tick_interval);

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
fn spawn_snapshot_loop(
    state: AppState,
    config: SnapshotConfig,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut snapshot = SnapshotLoop::new(config);
        let mut deriver = EventDeriver::new();
        let mut ticker = tokio::time::interval(interval);
        // A tick that overruns must not cause a burst of catch-up ticks; the
        // next one is simply late (`AGENTS.md` rule 7).
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

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

            let services: Vec<_> = snapshot.registry().services().cloned().collect();
            let connections = snapshot.topology().connections().to_vec();
            let projects = snapshot.projects().clone();

            let frames = state
                .apply_tick(&tick, &projects, services, connections, events)
                .await;

            debug!(
                frames = frames.len(),
                duration_ms = tick.collector_duration_ms,
                "tick applied"
            );

            for frame in frames {
                state.publish(frame);
            }
        }
    })
}
