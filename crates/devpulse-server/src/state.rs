//! Shared daemon state (tasks T3.1–T3.4).
//!
//! The snapshot loop is a `&mut` reconciler and the API is a set of concurrent
//! readers, so the two are separated by this module: the loop produces a view,
//! the state stores exactly one copy of it, and every HTTP or WebSocket reader
//! borrows that copy under a read lock.
//!
//! `AGENTS.md` rule 8 — Rust owns runtime truth — means this is the only place
//! the current world is stored. Nothing downstream recomputes it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use devpulse_core::ids::{ConnectionId, ProjectId, ServiceId};
use devpulse_core::model::{Connection, DevPulseEvent, Project, Service, Warning};
use devpulse_core::resources::ResourceHistory;
use devpulse_discovery::platform::{self, PlatformCapabilities};
use tokio::sync::{RwLock, RwLockReadGuard, broadcast};
use tracing::warn;

use crate::dto::{
    CollectorStatusDto, CollectorsDto, ConnectionDto, CountsDto, DockerStatusDto,
    ProjectSummaryDto, ServiceDto, StatusDto, WarningDto, rfc3339,
};
use crate::frames::ServerFrame;
use crate::snapshot::{CollectorTiming, TickResult};

/// Events kept in memory for the `/api/v1/events` endpoint. SQLite is the
/// durable store (Milestone 5); this ring only has to cover what a dashboard
/// shows without a query round trip.
pub const EVENT_RING_CAPACITY: usize = 2_000;

/// Frames a WebSocket client may fall behind by before it is dropped. A slow
/// consumer is disconnected rather than buffered without bound
/// (`docs/api-contract.md`); it reconnects and gets a fresh snapshot.
pub const BROADCAST_CAPACITY: usize = 256;

/// The current world, as of the last completed tick.
#[derive(Debug, Default)]
pub struct RuntimeView {
    pub projects: BTreeMap<ProjectId, Project>,
    pub services: BTreeMap<ServiceId, Service>,
    pub connections: Vec<Connection>,
    /// Empty until the warning rules land (Milestone 7). Present on the wire
    /// from the start so the dashboard contract does not change later.
    pub warnings: Vec<Warning>,
    /// Newest last.
    pub events: VecDeque<DevPulseEvent>,
    pub resources: ResourceHistory,
    pub process_collector: CollectorTiming,
    pub socket_collector: CollectorTiming,
    /// `None` until the first tick completes.
    pub last_tick: Option<SystemTime>,
}

impl RuntimeView {
    pub fn services_of<'a>(&'a self, project: &'a ProjectId) -> impl Iterator<Item = &'a Service> {
        self.services
            .values()
            .filter(move |s| s.project_id.as_ref() == Some(project))
    }

    /// Edges with both ends inside `project`, plus any edge that leaves it: a
    /// dependency on someone else's database is exactly what a developer needs
    /// to see.
    pub fn connections_of(&self, project: &ProjectId) -> Vec<&Connection> {
        let member = |id: &ServiceId| {
            self.services
                .get(id)
                .and_then(|s| s.project_id.as_ref())
                .is_some_and(|p| p == project)
        };
        self.connections
            .iter()
            .filter(|c| member(&c.source) || member(&c.target))
            .collect()
    }

    pub fn connections_touching(
        &self,
        service: &ServiceId,
    ) -> (Vec<&Connection>, Vec<&Connection>) {
        let outbound = self.connections.iter().filter(|c| &c.source == service);
        let inbound = self.connections.iter().filter(|c| &c.target == service);
        (outbound.collect(), inbound.collect())
    }

    pub fn warnings_of(&self, project: &ProjectId) -> Vec<&Warning> {
        self.warnings
            .iter()
            .filter(|w| w.project_id.as_ref() == Some(project))
            .collect()
    }

    /// Most recent warning for a project, or `None`.
    pub fn latest_warning(&self, project: &ProjectId) -> Option<&Warning> {
        self.warnings_of(project)
            .into_iter()
            .max_by_key(|w| w.last_seen)
    }

    /// Events newest first, filtered and capped by the caller's query.
    pub fn recent_events(&self, filter: &EventFilter) -> Vec<&DevPulseEvent> {
        self.events
            .iter()
            .rev()
            .filter(|e| filter.matches(e, &self.services))
            .take(filter.limit)
            .collect()
    }

    /// Wire shape for one service, with its resource history attached.
    pub fn service_dto(&self, service: &Service) -> ServiceDto {
        ServiceDto::new(service, &self.resources.history(&service.id))
    }

    pub fn project_summary(&self, project: &Project) -> ProjectSummaryDto {
        let services: Vec<&Service> = self.services_of(&project.id).collect();
        ProjectSummaryDto::new(project, &services, self.latest_warning(&project.id))
    }

    pub fn project_summaries(&self) -> Vec<ProjectSummaryDto> {
        self.projects
            .values()
            .map(|p| self.project_summary(p))
            .collect()
    }

    /// The project an edge belongs to: its source's project, since an edge is
    /// drawn from the caller's point of view. `None` when the source is not
    /// grouped (or is already gone).
    fn connection_project(&self, connection: &ConnectionId) -> Option<ProjectId> {
        self.connections
            .iter()
            .find(|c| &c.id == connection)
            .and_then(|c| self.services.get(&c.source))
            .and_then(|s| s.project_id.clone())
    }

    fn push_event(&mut self, event: DevPulseEvent) {
        if self.events.len() == EVENT_RING_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

/// Query for `/api/v1/events` and for the per-entity `recent_events` fields.
#[derive(Debug, Clone)]
pub struct EventFilter {
    pub project_id: Option<ProjectId>,
    pub service_id: Option<ServiceId>,
    pub since: Option<SystemTime>,
    pub limit: usize,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            project_id: None,
            service_id: None,
            since: None,
            limit: DEFAULT_EVENT_LIMIT,
        }
    }
}

pub const DEFAULT_EVENT_LIMIT: usize = 100;
pub const MAX_EVENT_LIMIT: usize = 1_000;

impl EventFilter {
    pub fn for_project(project: &ProjectId, limit: usize) -> Self {
        Self {
            project_id: Some(project.clone()),
            limit,
            ..Self::default()
        }
    }

    pub fn for_service(service: &ServiceId, limit: usize) -> Self {
        Self {
            service_id: Some(service.clone()),
            limit,
            ..Self::default()
        }
    }

    fn matches(&self, event: &DevPulseEvent, services: &BTreeMap<ServiceId, Service>) -> bool {
        if let Some(since) = self.since
            && event.at < since
        {
            return false;
        }
        if let Some(project) = &self.project_id {
            // An event carries its project directly; fall back to the service's
            // current project so a service that was grouped late still matches.
            let direct = event.project_id.as_ref() == Some(project);
            let via_service = event_service(event)
                .and_then(|id| services.get(id))
                .and_then(|s| s.project_id.as_ref())
                .is_some_and(|p| p == project);
            if !direct && !via_service {
                return false;
            }
        }
        if let Some(service) = &self.service_id
            && event_service(event) != Some(service)
        {
            return false;
        }
        true
    }
}

/// The service an event is about, if it is about one.
pub fn event_service(event: &DevPulseEvent) -> Option<&ServiceId> {
    use devpulse_core::model::EventKind::*;
    match &event.kind {
        ServiceStarted { service_id, .. }
        | ServiceStopped { service_id, .. }
        | ServiceRestarted { service_id, .. }
        | HealthChanged { service_id, .. }
        | ResourceWarning { service_id, .. } => Some(service_id),
        // A port can open before DevPulse knows whose it is.
        PortOpened { service_id, .. } | PortClosed { service_id, .. } => service_id.as_ref(),
        ConnectionStarted { source, .. } => Some(source),
        ProjectDetected { .. } | ConnectionEnded { .. } | FileChanged { .. } => None,
    }
}

/// Everything an API handler needs. Cloned freely: it is an `Arc` inside.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    started_at: SystemTime,
    version: &'static str,
    platform: PlatformCapabilities,
    docker: DockerStatusDto,
    view: RwLock<RuntimeView>,
    frames: broadcast::Sender<Arc<str>>,
}

impl AppState {
    pub fn new(docker: DockerStatusDto) -> Self {
        let (frames, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self(Arc::new(Inner {
            started_at: SystemTime::now(),
            version: env!("CARGO_PKG_VERSION"),
            platform: platform::capabilities(),
            docker,
            view: RwLock::new(RuntimeView::default()),
            frames,
        }))
    }

    /// Docker is not probed here: detection is async and belongs to startup,
    /// so the daemon passes the answer in.
    pub fn docker_unknown() -> Self {
        Self::new(DockerStatusDto {
            available: false,
            reason: Some("not probed".to_string()),
        })
    }

    pub async fn view(&self) -> RwLockReadGuard<'_, RuntimeView> {
        self.0.view.read().await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.0.frames.subscribe()
    }

    /// Publish a pre-serialised frame. Failure means nobody is listening,
    /// which is the normal case for a daemon with no dashboard open.
    pub fn publish(&self, frame: Arc<str>) {
        let _ = self.0.frames.send(frame);
    }

    pub fn started_at(&self) -> SystemTime {
        self.0.started_at
    }

    /// `/api/v1/status`. Reads the view, so it reflects the last completed
    /// tick; before the first tick the counts are zero and `last_run` is null.
    pub async fn status(&self) -> StatusDto {
        let view = self.view().await;
        self.status_of(&view)
    }

    /// Status against a view the caller already holds a guard for. Taking the
    /// lock twice would risk deadlocking against a waiting writer.
    pub fn status_of(&self, view: &RuntimeView) -> StatusDto {
        StatusDto {
            version: self.0.version.to_string(),
            started_at: rfc3339(self.0.started_at),
            uptime_ms: SystemTime::now()
                .duration_since(self.0.started_at)
                .unwrap_or_default()
                .as_millis() as u64,
            platform: serde_json::to_value(&self.0.platform).unwrap_or(serde_json::Value::Null),
            docker: self.0.docker.clone(),
            counts: CountsDto {
                projects: view.projects.len(),
                services: view.services.len(),
                connections: view.connections.len(),
                events: view.events.len(),
            },
            collectors: CollectorsDto {
                process: collector_dto(&view.process_collector),
                socket: collector_dto(&view.socket_collector),
            },
        }
    }

    /// Fold one tick into the view and return the frames it produced, already
    /// serialised. Frames are built here because this is the only place that
    /// sees both the previous and the new world — mapping a removed edge back
    /// to its project needs the old one.
    ///
    /// Broadcasting is left to the caller so the write lock is never held
    /// across a send.
    pub async fn apply_tick(
        &self,
        tick: &TickResult,
        projects: &BTreeMap<ProjectId, Project>,
        services: Vec<Service>,
        connections: Vec<Connection>,
        events: Vec<DevPulseEvent>,
    ) -> Vec<Arc<str>> {
        let at = rfc3339(tick.at.unwrap_or_else(SystemTime::now));
        let mut view = self.0.view.write().await;

        // Removed edges are resolved against the *old* topology, while it is
        // still there to resolve against.
        let mut removed_by_project: BTreeMap<Option<ProjectId>, Vec<String>> = BTreeMap::new();
        for id in &tick.topology_delta.removed {
            removed_by_project
                .entry(view.connection_project(id))
                .or_default()
                .push(id.to_string());
        }

        view.projects = projects.clone();
        view.services = services.into_iter().map(|s| (s.id.clone(), s)).collect();
        view.connections = connections;
        view.last_tick = tick.at;
        view.process_collector = tick.process.clone();
        view.socket_collector = tick.socket.clone();

        for (service, sample) in &tick.samples {
            view.resources.record(service, *sample);
        }
        for evicted in &tick.registry_delta.evicted {
            view.resources.forget(evicted);
        }
        for event in &events {
            view.push_event(event.clone());
        }

        let mut frames = Vec::new();

        let changed = changed_services(&tick.registry_delta);
        let removed: Vec<String> = tick
            .registry_delta
            .evicted
            .iter()
            .map(ToString::to_string)
            .collect();
        if !changed.is_empty() || !removed.is_empty() {
            // A changed service that is no longer in the registry was evicted
            // in the same tick; `removed` already covers it.
            let services: Vec<ServiceDto> = changed
                .iter()
                .filter_map(|id| view.services.get(id))
                .map(|s| view.service_dto(s))
                .collect();
            frames.push(ServerFrame::ServicesChanged {
                at: at.clone(),
                services,
                removed,
            });
        }

        // Added edges group by their source's project; a `None` key is an edge
        // DevPulse cannot attribute, and it is still reported.
        let mut added_by_project: BTreeMap<Option<ProjectId>, Vec<ConnectionDto>> = BTreeMap::new();
        for connection in &tick.topology_delta.added {
            let project = view
                .services
                .get(&connection.source)
                .and_then(|s| s.project_id.clone());
            added_by_project
                .entry(project)
                .or_default()
                .push(ConnectionDto::from(connection));
        }
        let projects_touched: std::collections::BTreeSet<Option<ProjectId>> = added_by_project
            .keys()
            .chain(removed_by_project.keys())
            .cloned()
            .collect();
        for project in projects_touched {
            frames.push(ServerFrame::TopologyChanged {
                at: at.clone(),
                project_id: project.as_ref().map(ToString::to_string),
                added: added_by_project.remove(&project).unwrap_or_default(),
                removed: removed_by_project.remove(&project).unwrap_or_default(),
            });
        }

        if !events.is_empty() {
            frames.push(ServerFrame::Events {
                at,
                events: events.iter().map(Into::into).collect(),
            });
        }

        drop(view);
        frames.iter().filter_map(|f| self.encode(f)).collect()
    }

    /// The one frame every client gets on connect.
    pub async fn snapshot_frame(&self) -> ServerFrame {
        let view = self.view().await;
        ServerFrame::Snapshot {
            at: rfc3339(view.last_tick.unwrap_or_else(SystemTime::now)),
            status: Box::new(self.status_of(&view)),
            projects: view.project_summaries(),
            services: view
                .services
                .values()
                .map(|s| view.service_dto(s))
                .collect(),
            connections: view.connections.iter().map(ConnectionDto::from).collect(),
            warnings: view.warnings.iter().map(WarningDto::from).collect(),
        }
    }

    /// Serialise a frame once for every subscriber. A frame that cannot be
    /// serialised is dropped with a log line rather than killing the tick.
    pub fn encode(&self, frame: &impl serde::Serialize) -> Option<Arc<str>> {
        match serde_json::to_string(frame) {
            Ok(json) => Some(Arc::from(json.as_str())),
            Err(error) => {
                warn!(%error, "dropping unserialisable frame");
                None
            }
        }
    }
}

/// Services whose observable state changed this tick, deduplicated.
fn changed_services(delta: &devpulse_core::registry::RegistryDelta) -> Vec<ServiceId> {
    let mut ids: Vec<ServiceId> = delta
        .started
        .iter()
        .map(|(id, _)| id.clone())
        .chain(delta.stopped.iter().map(|(id, _)| id.clone()))
        .chain(delta.restarted.iter().map(|(id, _, _)| id.clone()))
        .chain(delta.ports_opened.iter().map(|(id, _)| id.clone()))
        .chain(delta.ports_closed.iter().map(|(id, _)| id.clone()))
        .chain(delta.health_changed.iter().map(|(id, _, _)| id.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn collector_dto(timing: &CollectorTiming) -> CollectorStatusDto {
    CollectorStatusDto {
        last_duration_ms: timing.duration_ms,
        last_run: timing.last_run.map(rfc3339),
        degraded_fields: timing.degraded_fields.clone(),
        sockets_without_owner: timing.sockets_without_owner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devpulse_core::ids::EventId;
    use devpulse_core::model::EventKind;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
    }

    fn started(service: &str, secs: u64) -> DevPulseEvent {
        DevPulseEvent {
            id: EventId::new(1_700_000_000_000 + secs * 1_000, secs as u32),
            at: at(secs),
            project_id: Some(ProjectId::derived("/tmp/one")),
            kind: EventKind::ServiceStarted {
                service_id: ServiceId::derived(service),
                pid: 1,
            },
        }
    }

    #[test]
    fn the_event_ring_is_bounded() {
        let mut view = RuntimeView::default();
        for i in 0..(EVENT_RING_CAPACITY + 50) {
            view.push_event(started("svc", i as u64));
        }
        assert_eq!(view.events.len(), EVENT_RING_CAPACITY);
        // The oldest events are the ones dropped.
        assert_eq!(view.events.front().expect("front").at, at(50));
    }

    #[test]
    fn events_come_back_newest_first() {
        let mut view = RuntimeView::default();
        view.push_event(started("svc", 1));
        view.push_event(started("svc", 2));
        view.push_event(started("svc", 3));

        let recent = view.recent_events(&EventFilter::default());
        let times: Vec<_> = recent.iter().map(|e| e.at).collect();
        assert_eq!(times, vec![at(3), at(2), at(1)]);
    }

    #[test]
    fn filters_by_service_and_since() {
        let mut view = RuntimeView::default();
        view.push_event(started("a", 1));
        view.push_event(started("b", 2));
        view.push_event(started("a", 3));

        let only_a = EventFilter::for_service(&ServiceId::derived("a"), 10);
        assert_eq!(view.recent_events(&only_a).len(), 2);

        let since = EventFilter {
            since: Some(at(3)),
            ..EventFilter::default()
        };
        assert_eq!(view.recent_events(&since).len(), 1);
    }

    #[test]
    fn limit_caps_the_result() {
        let mut view = RuntimeView::default();
        for i in 0..10 {
            view.push_event(started("svc", i));
        }
        let filter = EventFilter {
            limit: 3,
            ..EventFilter::default()
        };
        assert_eq!(view.recent_events(&filter).len(), 3);
    }

    #[tokio::test]
    async fn status_is_answerable_before_the_first_tick() {
        let state = AppState::docker_unknown();
        let status = state.status().await;
        assert_eq!(status.counts.services, 0);
        assert!(status.collectors.process.last_run.is_none());
        assert_eq!(status.platform["os"], std::env::consts::OS);
    }
}
