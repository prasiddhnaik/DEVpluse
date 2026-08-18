//! Live service registry (task T2.2).
//!
//! Reconciles each batch of observations against the services already known and
//! reports what changed. Three properties matter more here than anything else:
//!
//! * **A PID change is not a new service** (`DECISIONS.md` D006, `AGENTS.md`
//!   rule 5). Identity is the [`ServiceFingerprint`]; PIDs move underneath it.
//! * **A stopped service still exists.** The developer's mental model is "my API
//!   server is down", not "my API server vanished", so a stopped service is
//!   retained with its `first_seen` and `restart_count`. Its `instances` and
//!   `endpoints` are cleared, because those describe a process that is gone.
//! * **Retention is bounded.** Stopped listeners (and containers) are kept up
//!   to [`MAX_STOPPED_SERVICES`]; a stopped host process that never listened
//!   is dropped after [`STOPPED_PORTLESS_RETENTION`]. Nothing in this module
//!   grows with uptime (`AGENTS.md` rule 7).
//!
//! # What counts as a restart
//!
//! Both of these increment `restart_count`:
//!
//! * the same service is observed with a completely different PID set than last
//!   time (a fast in-place restart, caught inside one tick) — reported as
//!   [`RegistryDelta::restarted`], never as a stop followed by a start;
//! * a service that was retained in the stopped state is observed running again
//!   — reported as [`RegistryDelta::started`], because from the registry's point
//!   of view it did stop.
//!
//! The second case is what a crash loop actually looks like at a 1 Hz sample
//! rate, so it has to count. Collapsing the resulting stop/start pair into a
//! single user-visible restart event is the event deriver's job, not the
//! registry's: the registry reports observations, not narrative.
//!
//! A PID set that merely *overlaps* the previous one is not a restart. A
//! multi-process service (a Node cluster, a Python worker pool) constantly
//! gains and loses workers while the service itself never went down.

use std::collections::{BTreeMap, VecDeque, btree_map::Entry};
use std::time::{Duration, SystemTime};

use crate::identity::{Runtime, ServiceFingerprint};
use crate::ids::{ProjectId, ServiceId};
use crate::model::{Endpoint, Health, ProcessInstance, Protocol, Service, ServiceKind};

/// How many stopped services are retained. 256 is far more than a developer
/// machine accumulates in a session, and the memory cost is a few hundred
/// kilobytes at worst, so the bound is invisible in practice while still being
/// a hard bound.
pub const MAX_STOPPED_SERVICES: usize = 256;

/// How long a stopped host process that never listened is kept.
///
/// A listener that dies is "my API is down" and belongs in the listing. A
/// portless worker that dies is usually a one-shot: a test binary, a helper,
/// a script that lived just long enough to pass [`crate::service_filter::MIN_PORTLESS_LIFETIME`].
/// Keeping those for the full [`MAX_STOPPED_SERVICES`] cap is how a project
/// card ends up reading `9/68`.
///
/// The window is longer than [`RESTART_LOOP_WINDOW`] so a crashing worker is
/// still seen as a restart loop rather than a new service every time. After
/// that, it is gone. Containers are never aged out this way: a container with
/// no published port is still "my postgres is down".
pub const STOPPED_PORTLESS_RETENTION: Duration = Duration::from_secs(90);

/// Window in which repeated restarts mean "restart loop" rather than "I
/// restarted my dev server".
pub const RESTART_LOOP_WINDOW: Duration = Duration::from_secs(60);

/// Restarts inside [`RESTART_LOOP_WINDOW`] required to report
/// [`Health::Degraded`]. Two restarts in a minute is a developer editing code;
/// three is something crashing.
pub const RESTART_LOOP_THRESHOLD: usize = 3;

/// One running process, already attributed to a logical service.
///
/// A `ServiceObservation` always describes something that is running: the
/// collector saw the process. Absence of an observation is how the registry
/// learns a service stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceObservation {
    pub fingerprint: ServiceFingerprint,
    pub name: String,
    pub project_id: Option<ProjectId>,
    pub kind: ServiceKind,
    pub runtime: Runtime,
    /// `None` for a container: Docker does not disclose the host PIDs of the
    /// processes inside it, so there is no honest instance to report. Liveness
    /// for those comes from the observation existing at all.
    pub instance: Option<ProcessInstance>,
    pub endpoints: Vec<Endpoint>,
}

/// What changed between two batches.
///
/// Every vector is sorted by [`ServiceId`] so that two identical worlds always
/// produce an identical delta; the event deriver and the tests depend on that.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistryDelta {
    /// Service is running and was not running before. Carries the primary PID,
    /// or `None` for a container.
    pub started: Vec<(ServiceId, Option<u32>)>,
    /// Service is no longer observed. Carries the PID it last ran as.
    pub stopped: Vec<(ServiceId, Option<u32>)>,
    /// Same service, entirely new PID set: `(service, old_pid, new_pid)`.
    pub restarted: Vec<(ServiceId, Option<u32>, Option<u32>)>,
    pub ports_opened: Vec<(ServiceId, u16)>,
    pub ports_closed: Vec<(ServiceId, u16)>,
    pub health_changed: Vec<(ServiceId, Health, Health)>,
    /// Services dropped from the registry entirely because stopped-service
    /// retention was exceeded. Holders of per-service side tables (resource
    /// history, aliases in memory) must drop these ids too.
    ///
    /// Not part of the original milestone contract; added because eviction has
    /// to be observable somewhere or side tables leak.
    pub evicted: Vec<ServiceId>,
    /// Project each service currently belongs to, keyed by service id. The
    /// registry carries it through because a service can change project (rare)
    /// and because events need it; the caller supplies it on each observation.
    pub projects: BTreeMap<ServiceId, Option<ProjectId>>,
}

impl RegistryDelta {
    /// True when the world did not change at all — the common case at 1 Hz.
    pub fn is_empty(&self) -> bool {
        self.started.is_empty()
            && self.stopped.is_empty()
            && self.restarted.is_empty()
            && self.ports_opened.is_empty()
            && self.ports_closed.is_empty()
            && self.health_changed.is_empty()
            && self.evicted.is_empty()
    }

    fn sort(&mut self) {
        self.started.sort_unstable();
        self.stopped.sort_unstable();
        self.restarted.sort_unstable();
        self.ports_opened.sort_unstable();
        self.ports_closed.sort_unstable();
        // `Health` is deliberately not `Ord` (there is no meaningful order), so
        // sort on the id alone. Ids are unique in this vector.
        self.health_changed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        self.evicted.sort_unstable();
    }
}

/// A service plus the bookkeeping that is not part of its public shape.
#[derive(Debug, Clone)]
struct Entered {
    service: Service,
    /// Timestamps of the most recent restarts, capped at
    /// [`RESTART_LOOP_THRESHOLD`] entries — exactly what the loop test needs
    /// and nothing more, so this cannot grow.
    restarts: VecDeque<SystemTime>,
    /// Whether this service has ever owned a listening port. Endpoints are
    /// cleared on stop, so this flag is the only honest record that it was
    /// a listener — and therefore worth keeping as "my API is down".
    ever_listened: bool,
}

impl Entered {
    /// Listeners and containers stay until the hard cap; portless host
    /// workers are aged out after [`STOPPED_PORTLESS_RETENTION`].
    fn retain_when_stopped(&self) -> bool {
        self.ever_listened || matches!(self.service.kind, ServiceKind::Container(_))
    }

    fn record_restart(&mut self, at: SystemTime) {
        self.service.restart_count = self.service.restart_count.saturating_add(1);
        if self.restarts.len() == RESTART_LOOP_THRESHOLD {
            self.restarts.pop_front();
        }
        self.restarts.push_back(at);
    }

    /// Health of a running service: degraded while the last
    /// [`RESTART_LOOP_THRESHOLD`] restarts all fall inside
    /// [`RESTART_LOOP_WINDOW`], healthy otherwise. Recomputed every batch so
    /// the signal clears once the loop stops.
    fn running_health(&self) -> Health {
        let (Some(oldest), Some(newest)) = (self.restarts.front(), self.restarts.back()) else {
            return Health::Healthy;
        };
        if self.restarts.len() < RESTART_LOOP_THRESHOLD {
            return Health::Healthy;
        }
        match newest.duration_since(*oldest) {
            Ok(span) if span <= RESTART_LOOP_WINDOW => Health::Degraded,
            // A clock that went backwards is not evidence of a restart loop.
            _ => Health::Healthy,
        }
    }
}

/// Holds the current logical services and diffs observations against them.
#[derive(Debug, Default)]
pub struct ServiceRegistry {
    /// Ordered so iteration, and therefore every delta, is deterministic.
    services: BTreeMap<ServiceId, Entered>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile `observations` against the held state.
    ///
    /// `observations` is the *complete* set of running processes DevPulse could
    /// attribute to services. A service missing from it is treated as stopped,
    /// so a caller that filters observations will report spurious stops.
    pub fn apply(
        &mut self,
        observations: Vec<ServiceObservation>,
        at: SystemTime,
    ) -> RegistryDelta {
        let mut delta = RegistryDelta::default();

        // Several processes can back one logical service, so fold first.
        let mut observed: BTreeMap<ServiceId, Merged> = BTreeMap::new();
        for observation in observations {
            let id = observation.fingerprint.service_id();
            match observed.entry(id) {
                Entry::Vacant(slot) => {
                    slot.insert(Merged::new(observation));
                }
                Entry::Occupied(mut slot) => slot.get_mut().absorb(observation),
            }
        }

        // Which ids were observed must be recorded explicitly. Inferring it
        // from `last_seen == at` breaks whenever two batches share a timestamp,
        // which silently suppresses every stop in the second batch.
        let observed_ids: Vec<ServiceId> = observed.keys().cloned().collect();

        for (id, merged) in observed {
            let merged = merged.finish();
            match self.services.entry(id.clone()) {
                Entry::Vacant(slot) => {
                    for port in &merged.ports {
                        delta.ports_opened.push((id.clone(), *port));
                    }
                    delta.started.push((id.clone(), merged.primary_pid));
                    let ever_listened = !merged.ports.is_empty();
                    slot.insert(Entered {
                        service: merged.into_service(id, at),
                        restarts: VecDeque::new(),
                        ever_listened,
                    });
                }
                Entry::Occupied(mut slot) => {
                    Self::update(slot.get_mut(), &id, merged, at, &mut delta);
                }
            }
        }

        // Anything still marked running that we did not observe has stopped.
        for (id, entered) in &mut self.services {
            if observed_ids.binary_search(id).is_ok() || !entered.service.is_running() {
                continue;
            }
            let pid = primary_pid(&entered.service.instances);
            for endpoint in &entered.service.endpoints {
                delta.ports_closed.push((id.clone(), endpoint.port));
            }
            delta.ports_closed.dedup();
            entered.service.instances.clear();
            // A stopped service's endpoints described a process that no longer
            // exists; keeping them would render a dead service as listening.
            entered.service.endpoints.clear();
            entered.restarts.clear();
            let previous = entered.service.health;
            entered.service.health = Health::Stopped;
            if previous != Health::Stopped {
                delta
                    .health_changed
                    .push((id.clone(), previous, Health::Stopped));
            }
            delta.stopped.push((id.clone(), pid));
        }

        self.evict_stopped(&mut delta, at);

        // Carry each service's project membership so event derivation and the
        // API can attach events to projects without a second lookup. Evicted
        // services are gone from `self.services` and so missing here, which is
        // what callers want.
        for (id, entered) in &self.services {
            delta
                .projects
                .insert(id.clone(), entered.service.project_id.clone());
        }

        delta.sort();
        delta
    }

    fn update(
        entered: &mut Entered,
        id: &ServiceId,
        merged: Finished,
        at: SystemTime,
        delta: &mut RegistryDelta,
    ) {
        let was_running = entered.service.is_running();
        let previous_pids = pid_set(&entered.service.instances);
        let previous_primary = primary_pid(&entered.service.instances);
        if !merged.ports.is_empty() {
            entered.ever_listened = true;
        }

        if was_running {
            let overlaps = merged
                .instances
                .iter()
                .any(|i| previous_pids.contains(&i.pid));
            // A container has no instances on either side, so PID overlap says
            // nothing about it; treating "no overlap" as a restart there would
            // report a restart on every tick. Its restarts are seen as a
            // stop followed by a start instead.
            let pid_evidence = !merged.instances.is_empty() || !previous_pids.is_empty();
            if !overlaps && pid_evidence {
                entered.record_restart(at);
                delta
                    .restarted
                    .push((id.clone(), previous_primary, merged.primary_pid));
            }
        } else {
            // Known but stopped, now running again: it restarted between ticks.
            entered.record_restart(at);
            delta.started.push((id.clone(), merged.primary_pid));
        }

        diff_ports(id, &entered.service.endpoints, &merged.ports, delta);

        let service = &mut entered.service;
        // Display metadata can legitimately change (a container gets a Compose
        // label, a project is resolved on a later tick) even though identity
        // cannot; take the latest.
        service.name = merged.name;
        service.project_id = merged.project_id;
        service.kind = merged.kind;
        service.runtime = merged.runtime;
        service.instances = merged.instances;
        service.endpoints = merged.endpoints;
        service.last_seen = at;

        let health = entered.running_health();
        let previous = entered.service.health;
        if previous != health {
            entered.service.health = health;
            delta.health_changed.push((id.clone(), previous, health));
        }
    }

    /// Drop stopped services that no longer earn their keep.
    ///
    /// Portless host processes age out after [`STOPPED_PORTLESS_RETENTION`].
    /// Everything else — listeners and containers — is kept until the
    /// [`MAX_STOPPED_SERVICES`] cap, then the least recently seen go. Running
    /// services are never evicted.
    fn evict_stopped(&mut self, delta: &mut RegistryDelta, at: SystemTime) {
        let expired: Vec<ServiceId> = self
            .services
            .iter()
            .filter(|(_, entered)| {
                !entered.service.is_running()
                    && !entered.retain_when_stopped()
                    && at
                        .duration_since(entered.service.last_seen)
                        .is_ok_and(|age| age > STOPPED_PORTLESS_RETENTION)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.services.remove(&id);
            delta.evicted.push(id);
        }

        let stopped = self
            .services
            .values()
            .filter(|e| !e.service.is_running())
            .count();
        if stopped <= MAX_STOPPED_SERVICES {
            return;
        }

        let mut candidates: Vec<(SystemTime, ServiceId)> = self
            .services
            .iter()
            .filter(|(_, e)| !e.service.is_running())
            .map(|(id, e)| (e.service.last_seen, id.clone()))
            .collect();
        candidates.sort_unstable();

        for (_, id) in candidates.into_iter().take(stopped - MAX_STOPPED_SERVICES) {
            self.services.remove(&id);
            delta.evicted.push(id);
        }
    }

    /// Every service the registry knows, running or stopped, in id order.
    pub fn services(&self) -> impl Iterator<Item = &Service> {
        self.services.values().map(|e| &e.service)
    }

    pub fn get(&self, id: &ServiceId) -> Option<&Service> {
        self.services.get(id).map(|e| &e.service)
    }

    /// Number of services held, running and stopped. Bounded by
    /// [`MAX_STOPPED_SERVICES`] plus the number of live services.
    pub fn len(&self) -> usize {
        self.services.len()
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }
}

/// Observations for one service, being folded together.
struct Merged {
    /// PID that owns the display metadata, so folding is order-independent.
    /// `None` for a container, which has exactly one observation anyway.
    meta_pid: Option<u32>,
    name: String,
    project_id: Option<ProjectId>,
    kind: ServiceKind,
    runtime: Runtime,
    fingerprint: String,
    instances: Vec<ProcessInstance>,
    endpoints: Vec<Endpoint>,
}

/// A folded, normalised observation set.
struct Finished {
    primary_pid: Option<u32>,
    name: String,
    project_id: Option<ProjectId>,
    kind: ServiceKind,
    runtime: Runtime,
    fingerprint: String,
    instances: Vec<ProcessInstance>,
    endpoints: Vec<Endpoint>,
    /// Distinct listening TCP/UDP ports, ascending.
    ports: Vec<u16>,
}

impl Merged {
    fn new(observation: ServiceObservation) -> Self {
        Self {
            meta_pid: observation.instance.as_ref().map(|i| i.pid),
            name: observation.name,
            project_id: observation.project_id,
            kind: observation.kind,
            runtime: observation.runtime,
            fingerprint: observation.fingerprint.canonical().to_string(),
            instances: observation.instance.into_iter().collect(),
            endpoints: observation.endpoints,
        }
    }

    fn absorb(&mut self, observation: ServiceObservation) {
        // Lowest PID wins the display metadata. A pid-less observation (a
        // container) never displaces one that has a PID; two of them cannot
        // collide, because a container maps to exactly one fingerprint.
        let pid = observation.instance.as_ref().map(|i| i.pid);
        let wins = match (pid, self.meta_pid) {
            (Some(new), Some(current)) => new < current,
            (Some(_), None) => true,
            _ => false,
        };
        if wins {
            self.meta_pid = pid;
            self.name = observation.name;
            self.project_id = observation.project_id;
            self.kind = observation.kind;
            self.runtime = observation.runtime;
        }
        self.instances.extend(observation.instance);
        self.endpoints.extend(observation.endpoints);
    }

    fn finish(mut self) -> Finished {
        self.instances.sort_unstable_by_key(|i| i.pid);
        self.instances.dedup_by_key(|i| i.pid);
        self.endpoints.sort_unstable_by_key(endpoint_key);
        self.endpoints.dedup_by_key(|e| endpoint_key(e));

        let mut ports: Vec<u16> = Vec::with_capacity(self.endpoints.len());
        for endpoint in &self.endpoints {
            if ports.last() != Some(&endpoint.port) {
                ports.push(endpoint.port);
            }
        }

        Finished {
            primary_pid: primary_pid(&self.instances).or(self.meta_pid),
            name: self.name,
            project_id: self.project_id,
            kind: self.kind,
            runtime: self.runtime,
            fingerprint: self.fingerprint,
            instances: self.instances,
            endpoints: self.endpoints,
            ports,
        }
    }
}

impl Finished {
    fn into_service(self, id: ServiceId, at: SystemTime) -> Service {
        Service {
            id,
            project_id: self.project_id,
            name: self.name,
            kind: self.kind,
            runtime: self.runtime,
            fingerprint: self.fingerprint,
            health: Health::Healthy,
            instances: self.instances,
            endpoints: self.endpoints,
            first_seen: at,
            last_seen: at,
            restart_count: 0,
        }
    }
}

/// Total order over endpoints. `Protocol` is not `Ord` (nothing about tcp/udp
/// is ordered), so it is projected onto a bool.
fn endpoint_key(endpoint: &Endpoint) -> (u16, std::net::IpAddr, bool, Option<u32>) {
    (
        endpoint.port,
        endpoint.address,
        matches!(endpoint.protocol, Protocol::Udp),
        endpoint.pid,
    )
}

/// The process a developer would call "the" process of the service: the oldest
/// one, tie-broken by PID so it is stable across ticks.
fn primary_pid(instances: &[ProcessInstance]) -> Option<u32> {
    instances
        .iter()
        .min_by_key(|i| (i.started_at_epoch_secs, i.pid))
        .map(|i| i.pid)
}

fn pid_set(instances: &[ProcessInstance]) -> Vec<u32> {
    let mut pids: Vec<u32> = instances.iter().map(|i| i.pid).collect();
    pids.sort_unstable();
    pids
}

/// Two-pointer diff over ascending, deduplicated port lists.
fn diff_ports(id: &ServiceId, previous: &[Endpoint], current: &[u16], delta: &mut RegistryDelta) {
    let mut before: Vec<u16> = previous.iter().map(|e| e.port).collect();
    before.sort_unstable();
    before.dedup();

    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < current.len() {
        match (before.get(i), current.get(j)) {
            (Some(old), Some(new)) if old == new => {
                i += 1;
                j += 1;
            }
            (Some(old), Some(new)) if old < new => {
                delta.ports_closed.push((id.clone(), *old));
                i += 1;
            }
            (Some(_), Some(new)) => {
                delta.ports_opened.push((id.clone(), *new));
                j += 1;
            }
            (Some(old), None) => {
                delta.ports_closed.push((id.clone(), *old));
                i += 1;
            }
            (None, Some(new)) => {
                delta.ports_opened.push((id.clone(), *new));
                j += 1;
            }
            (None, None) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    const EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        EPOCH + Duration::from_secs(secs)
    }

    fn instance(pid: u32, started: u64) -> ProcessInstance {
        ProcessInstance {
            pid,
            parent_pid: None,
            executable: Some(PathBuf::from("/usr/local/bin/node")),
            command: vec!["node".to_string(), "server.js".to_string()],
            cwd: Some(PathBuf::from("/work/api")),
            started_at_epoch_secs: started,
            cpu_percent: 1.5,
            memory_bytes: 64 * 1024 * 1024,
        }
    }

    fn endpoint(port: u16) -> Endpoint {
        Endpoint {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            protocol: Protocol::Tcp,
            pid: None,
        }
    }

    /// Same fingerprint inputs => same service, whatever the PID.
    fn observation(name: &str, port: Option<u16>, pid: u32, started: u64) -> ServiceObservation {
        let fingerprint = ServiceFingerprint::host(
            None,
            Runtime::Node,
            Some(Path::new(&format!("/usr/local/bin/{name}"))),
            Some(Path::new("/work/api")),
            port,
        );
        ServiceObservation {
            fingerprint,
            name: name.to_string(),
            project_id: None,
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Node,
            instance: Some(instance(pid, started)),
            endpoints: port.map(endpoint).into_iter().collect(),
        }
    }

    use std::path::Path;

    #[test]
    fn start_reports_started_and_ports() {
        let mut registry = ServiceRegistry::new();
        let obs = observation("api", Some(3000), 100, 10);
        let id = obs.fingerprint.service_id();

        let delta = registry.apply(vec![obs], at(10));

        assert_eq!(delta.started, vec![(id.clone(), Some(100))]);
        assert_eq!(delta.ports_opened, vec![(id.clone(), 3000)]);
        assert!(delta.stopped.is_empty());
        assert!(delta.restarted.is_empty());
        let service = registry.get(&id).expect("registered");
        assert_eq!(service.health, Health::Healthy);
        assert_eq!(service.restart_count, 0);
        assert_eq!(service.first_seen, at(10));
    }

    #[test]
    fn stop_retains_the_service_with_its_restart_count() {
        let mut registry = ServiceRegistry::new();
        let id = observation("api", Some(3000), 100, 10)
            .fingerprint
            .service_id();

        registry.apply(vec![observation("api", Some(3000), 100, 10)], at(10));
        // Restart across ticks so there is a count worth preserving.
        registry.apply(vec![], at(11));
        registry.apply(vec![observation("api", Some(3000), 200, 12)], at(12));
        let delta = registry.apply(vec![], at(13));

        assert_eq!(delta.stopped, vec![(id.clone(), Some(200))]);
        assert_eq!(delta.ports_closed, vec![(id.clone(), 3000)]);
        assert_eq!(
            delta.health_changed,
            vec![(id.clone(), Health::Healthy, Health::Stopped)]
        );

        let service = registry.get(&id).expect("retained while stopped");
        assert_eq!(service.health, Health::Stopped);
        assert_eq!(service.restart_count, 1);
        assert_eq!(service.first_seen, at(10), "first_seen survives the outage");
        assert!(service.instances.is_empty());
        assert!(
            service.endpoints.is_empty(),
            "a dead service must not be rendered as listening"
        );
    }

    #[test]
    fn identity_survives_a_pid_change() {
        let mut registry = ServiceRegistry::new();
        let id = observation("api", Some(3000), 100, 10)
            .fingerprint
            .service_id();

        registry.apply(vec![observation("api", Some(3000), 100, 10)], at(10));
        let delta = registry.apply(vec![observation("api", Some(3000), 777, 11)], at(11));

        assert_eq!(delta.restarted, vec![(id.clone(), Some(100), Some(777))]);
        assert!(
            delta.started.is_empty() && delta.stopped.is_empty(),
            "an in-place restart is one event, not a stop plus a start"
        );
        assert_eq!(registry.len(), 1, "no second service was created");
        let service = registry.get(&id).expect("same id");
        assert_eq!(service.restart_count, 1);
        assert_eq!(service.instances.len(), 1);
        assert_eq!(service.instances[0].pid, 777);
    }

    #[test]
    fn overlapping_pid_set_is_worker_churn_not_a_restart() {
        let mut registry = ServiceRegistry::new();
        let mut primary = observation("api", Some(3000), 100, 10);
        let mut worker = observation("api", Some(3000), 101, 10);
        worker.endpoints.clear();
        let id = primary.fingerprint.service_id();

        registry.apply(vec![primary.clone(), worker], at(10));

        // Worker 101 replaced by 102; the listening primary is untouched.
        let mut replacement = observation("api", Some(3000), 102, 11);
        replacement.endpoints.clear();
        primary.instance = Some(instance(100, 10));
        let delta = registry.apply(vec![primary, replacement], at(11));

        assert!(delta.restarted.is_empty());
        assert!(delta.started.is_empty());
        assert_eq!(registry.get(&id).map(|s| s.restart_count), Some(0));
    }

    #[test]
    fn restart_across_ticks_counts_and_reports_started() {
        let mut registry = ServiceRegistry::new();
        let id = observation("api", Some(3000), 100, 10)
            .fingerprint
            .service_id();

        registry.apply(vec![observation("api", Some(3000), 100, 10)], at(10));
        registry.apply(vec![], at(11));
        let delta = registry.apply(vec![observation("api", Some(3000), 200, 12)], at(12));

        assert_eq!(delta.started, vec![(id.clone(), Some(200))]);
        assert_eq!(delta.ports_opened, vec![(id.clone(), 3000)]);
        assert_eq!(
            delta.health_changed,
            vec![(id.clone(), Health::Stopped, Health::Healthy)]
        );
        assert_eq!(registry.get(&id).map(|s| s.restart_count), Some(1));
    }

    #[test]
    fn new_port_and_lost_port_are_reported() {
        let mut registry = ServiceRegistry::new();
        // Keep the fingerprint stable by keeping the primary port stable and
        // adding a second endpoint by hand.
        let base = observation("api", Some(3000), 100, 10);
        let id = base.fingerprint.service_id();
        registry.apply(vec![base.clone()], at(10));

        let mut grown = base.clone();
        grown.endpoints.push(endpoint(9229));
        let delta = registry.apply(vec![grown], at(11));
        assert_eq!(delta.ports_opened, vec![(id.clone(), 9229)]);
        assert!(delta.ports_closed.is_empty());

        let delta = registry.apply(vec![base], at(12));
        assert_eq!(delta.ports_closed, vec![(id.clone(), 9229)]);
        assert!(delta.ports_opened.is_empty());
    }

    #[test]
    fn three_restarts_inside_the_window_are_degraded() {
        let mut registry = ServiceRegistry::new();
        let id = observation("api", Some(3000), 100, 10)
            .fingerprint
            .service_id();
        registry.apply(vec![observation("api", Some(3000), 100, 0)], at(0));

        for (tick, pid) in [(1, 101), (2, 102), (3, 103)] {
            let delta = registry.apply(vec![observation("api", Some(3000), pid, tick)], at(tick));
            if tick < 3 {
                assert!(
                    delta.health_changed.is_empty(),
                    "a developer restarting twice is not degraded"
                );
            }
        }

        let service = registry.get(&id).expect("present");
        assert_eq!(service.health, Health::Degraded);
        assert_eq!(service.restart_count, 3);
    }

    #[test]
    fn restarts_spread_beyond_the_window_stay_healthy() {
        let mut registry = ServiceRegistry::new();
        let id = observation("api", Some(3000), 100, 0)
            .fingerprint
            .service_id();
        registry.apply(vec![observation("api", Some(3000), 100, 0)], at(0));

        for (tick, pid) in [(30, 101), (61, 102), (200, 103)] {
            registry.apply(vec![observation("api", Some(3000), pid, tick)], at(tick));
        }

        assert_eq!(
            registry.get(&id).map(|s| s.health),
            Some(Health::Healthy),
            "restarts 30+ seconds apart are not a loop"
        );
    }

    #[test]
    fn several_processes_fold_into_one_service() {
        let mut registry = ServiceRegistry::new();
        let primary = observation("api", Some(3000), 100, 10);
        let id = primary.fingerprint.service_id();
        let mut worker = observation("api", Some(3000), 101, 10);
        worker.endpoints = vec![endpoint(3000)];

        let delta = registry.apply(vec![worker, primary], at(10));

        assert_eq!(registry.len(), 1);
        assert_eq!(delta.started, vec![(id.clone(), Some(100))]);
        assert_eq!(
            delta.ports_opened,
            vec![(id.clone(), 3000)],
            "the same port seen twice is one port"
        );
        let service = registry.get(&id).expect("present");
        assert_eq!(service.instances.len(), 2);
        assert_eq!(service.endpoints.len(), 1);
    }

    #[test]
    fn stopped_retention_is_bounded_and_evicts_least_recently_seen() {
        let mut registry = ServiceRegistry::new();
        let total = MAX_STOPPED_SERVICES + 10;

        let all: Vec<_> = (0..total)
            .map(|i| {
                observation(
                    "api",
                    Some(3000 + u16::try_from(i).expect("fits")),
                    1000 + u32::try_from(i).expect("fits"),
                    0,
                )
            })
            .collect();
        let ids: Vec<_> = all.iter().map(|o| o.fingerprint.service_id()).collect();

        // Every service running at once.
        registry.apply(all.clone(), at(0));
        assert_eq!(registry.len(), total);

        // Stop them one at a time, in index order, so each stop lands at a
        // distinct time and "least recently seen" is unambiguous.
        let mut total_evicted = 0usize;
        for stopped_upto in 1..=total {
            let still_running = all[stopped_upto..].to_vec();
            let delta = registry.apply(
                still_running,
                at(u64::try_from(stopped_upto).expect("fits")),
            );
            total_evicted += delta.evicted.len();
        }

        assert_eq!(
            registry.len(),
            MAX_STOPPED_SERVICES,
            "retention must bound the stopped set"
        );
        assert_eq!(total_evicted, total - MAX_STOPPED_SERVICES);
        assert!(
            registry.get(&ids[0]).is_none(),
            "the first service to stop is the first evicted"
        );
        assert!(
            registry.get(&ids[total - 1]).is_some(),
            "the most recently stopped service is retained"
        );
    }

    #[test]
    fn a_stopped_portless_worker_is_dropped_after_the_retention_window() {
        let mut registry = ServiceRegistry::new();
        let worker = observation("worker", None, 400, 10);
        let id = worker.fingerprint.service_id();

        registry.apply(vec![worker], at(10));
        let just_stopped = registry.apply(vec![], at(11));
        assert_eq!(just_stopped.stopped, vec![(id.clone(), Some(400))]);
        assert!(
            registry.get(&id).is_some(),
            "a just-stopped worker must stay long enough for restart detection"
        );
        assert!(just_stopped.evicted.is_empty());

        // last_seen is the last running observation (t=10), not the stop tick.
        let still_inside = registry.apply(vec![], at(10 + STOPPED_PORTLESS_RETENTION.as_secs()));
        assert!(
            registry.get(&id).is_some(),
            "the window is exclusive: at exactly {STOPPED_PORTLESS_RETENTION:?} of silence it is still kept"
        );
        assert!(still_inside.evicted.is_empty());

        let aged_out = registry.apply(vec![], at(11 + STOPPED_PORTLESS_RETENTION.as_secs()));
        assert!(
            registry.get(&id).is_none(),
            "a portless worker is not 'my API is down'"
        );
        assert_eq!(aged_out.evicted, vec![id]);
    }

    #[test]
    fn a_stopped_listener_outlives_the_portless_window() {
        let mut registry = ServiceRegistry::new();
        let api = observation("api", Some(3000), 100, 10);
        let id = api.fingerprint.service_id();

        registry.apply(vec![api], at(10));
        registry.apply(vec![], at(11));
        registry.apply(vec![], at(11 + STOPPED_PORTLESS_RETENTION.as_secs() * 10));

        let service = registry.get(&id).expect("a dead API is still the API");
        assert_eq!(service.health, Health::Stopped);
    }

    #[test]
    fn a_worker_that_later_listens_is_kept_when_it_stops() {
        let mut registry = ServiceRegistry::new();
        let mut worker = observation("worker", None, 400, 10);
        let id = worker.fingerprint.service_id();

        registry.apply(vec![worker.clone()], at(10));
        worker.endpoints = vec![endpoint(4100)];
        // Fingerprint includes the port, so this would normally be a new
        // service. Force the same identity by applying through the existing
        // id: absorb into the same observation shape the registry already
        // knows by reusing the original fingerprint.
        worker.fingerprint = observation("worker", None, 400, 10).fingerprint;
        registry.apply(vec![worker], at(11));
        registry.apply(vec![], at(12));
        registry.apply(vec![], at(12 + STOPPED_PORTLESS_RETENTION.as_secs() * 2));

        assert!(
            registry.get(&id).is_some(),
            "once it has listened, stopping it is an outage, not a cleanup"
        );
    }

    #[test]
    fn idle_batch_produces_an_empty_delta() {
        let mut registry = ServiceRegistry::new();
        registry.apply(vec![observation("api", Some(3000), 100, 10)], at(10));
        let delta = registry.apply(vec![observation("api", Some(3000), 100, 10)], at(11));
        assert!(delta.is_empty(), "unchanged world must produce no delta");
        assert_eq!(
            registry
                .get(
                    &delta
                        .started
                        .first()
                        .map(|(id, _)| id.clone())
                        .unwrap_or_else(|| observation("api", Some(3000), 100, 10)
                            .fingerprint
                            .service_id())
                )
                .map(|s| s.last_seen),
            Some(at(11))
        );
    }
}
