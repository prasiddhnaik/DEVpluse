//! Wire types for the local API (`docs/api-contract.md`).
//!
//! The domain model is not serialised directly. A separate DTO layer means the
//! HTTP contract can stay stable while the internal model evolves, and it keeps
//! internal-only fields off the wire. Timestamps become RFC 3339 strings here,
//! once, so no consumer has to guess an epoch unit.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devpulse_core::model::{
    Connection, DevPulseEvent, Endpoint, Evidence, Health, ProcessInstance, Project,
    ResourceSample, Service, ServiceKind, Warning,
};
use serde::{Deserialize, Serialize};

/// RFC 3339 UTC, second precision. Hand-rolled: the daemon has no date
/// dependency and this is the only place a calendar is needed.
pub fn rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let (year, month, day, hour, minute, second) = civil_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-from-civil algorithm (Howard Hinnant), valid for all proleptic
/// Gregorian dates. Used instead of a date crate because this is the only
/// calendar arithmetic in the daemon.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };

    (
        year,
        m as u32,
        d as u32,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// Parse the subset of RFC 3339 the API accepts: UTC only, `Z` suffix, with an
/// optional fractional part that is truncated to whole seconds. Anything else
/// is a `bad_request` rather than a silently wrong time.
pub fn parse_rfc3339(input: &str) -> Option<SystemTime> {
    let input = input.trim();
    let (date, rest) = input.split_once('T')?;
    let time = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z'))?;
    let time = time
        .split_once('.')
        .map_or(time, |(whole, _fraction)| whole);

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs < 0 {
        // Pre-epoch instants have no meaning for a daemon that started today.
        return Some(UNIX_EPOCH);
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Inverse of [`civil_from_unix`] (Howard Hinnant's days-from-civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceDto {
    pub evidence_type: String,
    pub confidence: f32,
    pub first_seen: String,
    pub last_seen: String,
    pub detail: Option<String>,
}

impl From<&Evidence> for EvidenceDto {
    fn from(e: &Evidence) -> Self {
        Self {
            evidence_type: serde_json::to_value(e.evidence_type)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "inferred".to_string()),
            confidence: e.confidence,
            first_seen: rfc3339(e.first_seen),
            last_seen: rfc3339(e.last_seen),
            detail: e.detail.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointDto {
    pub address: IpAddr,
    pub port: u16,
    pub protocol: String,
    pub pid: Option<u32>,
}

impl From<&Endpoint> for EndpointDto {
    fn from(e: &Endpoint) -> Self {
        Self {
            address: e.address,
            port: e.port,
            protocol: e.protocol.to_string(),
            pid: e.pid,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInstanceDto {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub executable: Option<PathBuf>,
    /// Already redacted at capture time; never raw argv.
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub started_at: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

impl From<&ProcessInstance> for ProcessInstanceDto {
    fn from(p: &ProcessInstance) -> Self {
        Self {
            pid: p.pid,
            parent_pid: p.parent_pid,
            executable: p.executable.clone(),
            command: p.command.clone(),
            cwd: p.cwd.clone(),
            started_at: rfc3339(UNIX_EPOCH + Duration::from_secs(p.started_at_epoch_secs)),
            cpu_percent: p.cpu_percent,
            memory_bytes: p.memory_bytes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSampleDto {
    pub at: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

impl From<&ResourceSample> for ResourceSampleDto {
    fn from(s: &ResourceSample) -> Self {
        Self {
            at: rfc3339(s.at),
            cpu_percent: s.cpu_percent,
            memory_bytes: s.memory_bytes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDto {
    pub id: String,
    pub source: String,
    pub target: String,
    pub target_port: u16,
    pub evidence: EvidenceDto,
}

impl From<&Connection> for ConnectionDto {
    fn from(c: &Connection) -> Self {
        Self {
            id: c.id.to_string(),
            source: c.source.to_string(),
            target: c.target.to_string(),
            target_port: c.target_port,
            evidence: EvidenceDto::from(&c.evidence),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDto {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub kind: serde_json::Value,
    pub runtime: String,
    pub fingerprint: String,
    pub health: String,
    pub restart_count: u32,
    pub first_seen: String,
    pub last_seen: String,
    pub instances: Vec<ProcessInstanceDto>,
    pub endpoints: Vec<EndpointDto>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resource_history: Vec<ResourceSampleDto>,
}

impl ServiceDto {
    pub fn new(service: &Service, history: &[ResourceSample]) -> Self {
        Self {
            id: service.id.to_string(),
            project_id: service.project_id.as_ref().map(ToString::to_string),
            name: service.name.clone(),
            kind: kind_json(&service.kind),
            runtime: service.runtime.as_str().to_string(),
            fingerprint: service.fingerprint.clone(),
            health: health_str(service.health).to_string(),
            restart_count: service.restart_count,
            first_seen: rfc3339(service.first_seen),
            last_seen: rfc3339(service.last_seen),
            instances: service.instances.iter().map(Into::into).collect(),
            endpoints: service.endpoints.iter().map(Into::into).collect(),
            resource_history: history.iter().map(Into::into).collect(),
        }
    }
}

fn kind_json(kind: &ServiceKind) -> serde_json::Value {
    serde_json::to_value(kind).unwrap_or_else(|_| serde_json::json!({ "kind": "host_process" }))
}

pub fn health_str(health: Health) -> &'static str {
    match health {
        Health::Healthy => "healthy",
        Health::Degraded => "degraded",
        Health::Stopped => "stopped",
        Health::Unknown => "unknown",
    }
}

/// Worst health wins: a project with one degraded service is degraded.
pub fn worst_health(healths: impl IntoIterator<Item = Health>) -> Health {
    let mut worst = Health::Unknown;
    let rank = |h: Health| match h {
        Health::Healthy => 0,
        Health::Unknown => 1,
        Health::Stopped => 2,
        Health::Degraded => 3,
    };
    let mut seen = false;
    for health in healths {
        if !seen || rank(health) > rank(worst) {
            worst = health;
            seen = true;
        }
    }
    worst
}

/// Health of a project as shown on the overview card.
///
/// Stopped services are history, not the current state of the work. A project
/// with a live API and a dead helper must read as healthy (or degraded), not
/// as stopped. A project with nothing running *is* stopped.
pub fn project_health<'a>(services: impl IntoIterator<Item = &'a Service>) -> Health {
    let services: Vec<&Service> = services.into_iter().collect();
    let running: Vec<Health> = services
        .iter()
        .filter(|service| service.is_running())
        .map(|service| service.health)
        .collect();
    if !running.is_empty() {
        worst_health(running)
    } else if services.is_empty() {
        Health::Unknown
    } else {
        Health::Stopped
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummaryDto {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub kind: String,
    pub confidence: f32,
    pub first_seen: String,
    pub last_seen: String,
    pub service_count: usize,
    pub running_service_count: usize,
    pub health: String,
    pub memory_bytes: u64,
    pub cpu_percent: f32,
    pub recent_warning: Option<WarningDto>,
}

impl ProjectSummaryDto {
    pub fn new(project: &Project, services: &[&Service], recent_warning: Option<&Warning>) -> Self {
        Self {
            id: project.id.to_string(),
            name: project.name.clone(),
            root: project.root.clone(),
            kind: project.kind.to_string().replace('-', "_"),
            confidence: project.confidence,
            first_seen: rfc3339(project.first_seen),
            last_seen: rfc3339(project.last_seen),
            service_count: services.len(),
            running_service_count: services.iter().filter(|s| s.is_running()).count(),
            health: health_str(project_health(services.iter().copied())).to_string(),
            memory_bytes: services.iter().map(|s| s.total_memory_bytes()).sum(),
            cpu_percent: services.iter().map(|s| s.total_cpu_percent()).sum(),
            recent_warning: recent_warning.map(WarningDto::from),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningDto {
    pub id: String,
    pub rule: String,
    pub severity: String,
    pub project_id: Option<String>,
    pub service_id: Option<String>,
    pub message: String,
    pub first_seen: String,
    pub last_seen: String,
    pub related_events: Vec<String>,
}

impl From<&Warning> for WarningDto {
    fn from(w: &Warning) -> Self {
        Self {
            id: w.id.clone(),
            rule: w.rule.clone(),
            severity: match w.severity {
                devpulse_core::Severity::Info => "info",
                devpulse_core::Severity::Warning => "warning",
                devpulse_core::Severity::Critical => "critical",
            }
            .to_string(),
            project_id: w.project_id.as_ref().map(ToString::to_string),
            service_id: w.service_id.as_ref().map(ToString::to_string),
            message: w.message.clone(),
            first_seen: rfc3339(w.first_seen),
            last_seen: rfc3339(w.last_seen),
            related_events: w.related_events.iter().map(ToString::to_string).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDto {
    pub id: String,
    pub at: String,
    pub project_id: Option<String>,
    pub kind: serde_json::Value,
}

impl From<&DevPulseEvent> for EventDto {
    fn from(e: &DevPulseEvent) -> Self {
        Self {
            id: e.id.to_string(),
            at: rfc3339(e.at),
            project_id: e.project_id.as_ref().map(ToString::to_string),
            kind: serde_json::to_value(&e.kind).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Node in the graph view. Deliberately flat and small: the graph endpoint
/// exists so the UI does not have to fetch every service to draw a picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeDto {
    pub id: String,
    pub name: String,
    pub runtime: String,
    pub health: String,
    pub port: Option<u16>,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub kind: String,
}

impl From<&Service> for GraphNodeDto {
    fn from(s: &Service) -> Self {
        Self {
            id: s.id.to_string(),
            name: s.name.clone(),
            runtime: s.runtime.as_str().to_string(),
            health: health_str(s.health).to_string(),
            port: s.primary_port(),
            cpu_percent: s.total_cpu_percent(),
            memory_bytes: s.total_memory_bytes(),
            kind: match s.kind {
                ServiceKind::HostProcess => "host_process".to_string(),
                ServiceKind::Container(_) => "container".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDto {
    pub project_id: String,
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<ConnectionDto>,
}

/// Machine-readable API error. `code` is a closed set so clients can branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBodyDto {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorDto {
    pub error: ApiErrorBodyDto,
}

impl ApiErrorDto {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBodyDto {
                code: code.to_string(),
                message: message.into(),
            },
        }
    }
}

/// Service detail: the service plus the things only the detail view needs
/// (`docs/api-contract.md` `GET /api/v1/services/:id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDetailDto {
    #[serde(flatten)]
    pub service: ServiceDto,
    pub connections: ServiceConnectionsDto,
    pub recent_events: Vec<EventDto>,
}

/// Edges split by direction so the inspector can say "calls" and "called by"
/// without the client re-deriving it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceConnectionsDto {
    pub outbound: Vec<ConnectionDto>,
    pub inbound: Vec<ConnectionDto>,
}

/// Everything the project view needs in one round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetailDto {
    pub project: ProjectSummaryDto,
    pub services: Vec<ServiceDto>,
    pub connections: Vec<ConnectionDto>,
    pub warnings: Vec<WarningDto>,
    pub recent_events: Vec<EventDto>,
}

/// One event in another's context (`GET /api/v1/events/:id/context`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedEventDto {
    #[serde(flatten)]
    pub event: EventDto,
    /// `same_service`, `same_project`, `preceding_file_change` or `temporal`.
    /// Never "caused by": DevPulse reports adjacency, not causation
    /// (`DECISIONS.md` D008).
    pub relation: String,
    /// Signed milliseconds from the anchor; negative is before it.
    pub offset_ms: i64,
}

/// What happened around one event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventContextDto {
    pub event: EventDto,
    pub window_ms: u64,
    /// Nearest first.
    pub before: Vec<RelatedEventDto>,
    /// Nearest first.
    pub after: Vec<RelatedEventDto>,
}

/// Whether Docker could be inspected, and if not, why — phrased for a human.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerStatusDto {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CountsDto {
    pub projects: usize,
    pub services: usize,
    pub connections: usize,
    pub events: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectorsDto {
    pub process: CollectorStatusDto,
    pub socket: CollectorStatusDto,
    /// Absent when the daemon has no Docker collector at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<CollectorStatusDto>,
}

/// `GET /api/v1/status`. Cheap enough to poll, and the only endpoint that is
/// meaningful before the first tick has completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusDto {
    pub version: String,
    pub started_at: String,
    pub uptime_ms: u64,
    /// [`devpulse_discovery::platform::PlatformCapabilities`], serialised as-is:
    /// the capability matrix is already the contract shape.
    pub platform: serde_json::Value,
    pub docker: DockerStatusDto,
    pub counts: CountsDto,
    pub collectors: CollectorsDto,
}

/// Collector health as reported by `/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectorStatusDto {
    pub last_duration_ms: u64,
    pub last_run: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub degraded_fields: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sockets_without_owner: Option<usize>,
    /// Why the last collection produced nothing, when it produced nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_epoch_as_rfc3339() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn formats_a_known_instant() {
        // 1700000000 = 2023-11-14T22:13:20Z
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(rfc3339(t), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn pre_epoch_times_do_not_panic() {
        let t = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(rfc3339(t), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn parses_what_it_formats() {
        for secs in [0u64, 1_700_000_000, 2_000_000_000] {
            let t = UNIX_EPOCH + Duration::from_secs(secs);
            assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
        }
    }

    #[test]
    fn parses_fractional_seconds_by_truncating() {
        let t = parse_rfc3339("2023-11-14T22:13:20.482Z").expect("parses");
        assert_eq!(rfc3339(t), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn rejects_anything_that_is_not_utc_rfc3339() {
        for bad in [
            "2023-11-14",
            "2023-11-14T22:13:20+01:00",
            "2023-13-14T22:13:20Z",
            "2023-11-14T25:13:20Z",
            "yesterday",
            "",
        ] {
            assert!(parse_rfc3339(bad).is_none(), "{bad} must not parse");
        }
    }

    #[test]
    fn worst_health_prefers_degraded_over_healthy() {
        assert_eq!(
            worst_health([Health::Healthy, Health::Degraded, Health::Healthy]),
            Health::Degraded
        );
        assert_eq!(
            worst_health([Health::Healthy, Health::Stopped]),
            Health::Stopped
        );
        assert_eq!(worst_health([Health::Healthy]), Health::Healthy);
        assert_eq!(worst_health([]), Health::Unknown);
    }

    fn host_service(health: Health, running: bool) -> Service {
        use devpulse_core::identity::Runtime;
        use devpulse_core::ids::ServiceId;
        use std::path::PathBuf;

        Service {
            id: ServiceId::derived("svc"),
            project_id: None,
            name: "api".into(),
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Native,
            fingerprint: "host".into(),
            health,
            instances: if running {
                vec![ProcessInstance {
                    pid: 1,
                    parent_pid: None,
                    executable: Some(PathBuf::from("/opt/app")),
                    command: vec!["api".into()],
                    cwd: None,
                    started_at_epoch_secs: 0,
                    cpu_percent: 0.0,
                    memory_bytes: 0,
                }]
            } else {
                vec![]
            },
            endpoints: vec![],
            first_seen: UNIX_EPOCH,
            last_seen: UNIX_EPOCH,
            restart_count: 0,
        }
    }

    #[test]
    fn project_health_does_not_let_a_dead_helper_paint_the_project_stopped() {
        let live = host_service(Health::Healthy, true);
        let dead = host_service(Health::Stopped, false);
        assert_eq!(project_health([&live, &dead]), Health::Healthy);

        let degraded = host_service(Health::Degraded, true);
        assert_eq!(project_health([&degraded, &dead]), Health::Degraded);
        assert_eq!(project_health([&dead]), Health::Stopped);
        assert_eq!(project_health([] as [&Service; 0]), Health::Unknown);
    }
}
