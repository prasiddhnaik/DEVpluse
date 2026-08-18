//! Deterministic warning rules (task T7.3).
//!
//! Every rule here is a threshold a developer can read, not a model they have
//! to trust (`DECISIONS.md` D008). A rule fires from observations Runscape
//! already holds — services, their resource history, and the events of the last
//! few minutes — and clears the moment its condition stops being true.
//!
//! Warning ids are stable per (rule, subject), so a rule that keeps firing
//! updates one warning rather than producing a new one every second.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use runscape_core::ids::{EventId, ProjectId, ServiceId};
use runscape_core::model::{
    EventKind, Health, ResourceSample, RunscapeEvent, Service, Severity, Warning,
};

/// Thresholds for the rules. Public so the daemon can loosen them and so the
/// tests can state exactly what they are asserting.
#[derive(Debug, Clone, PartialEq)]
pub struct WarningRules {
    /// Restarts within [`restart_window`](Self::restart_window) that mean a
    /// service is flapping rather than being restarted by a developer.
    pub restart_threshold: usize,
    pub restart_window: Duration,
    /// CPU percent (of one core) a service must hold for
    /// [`cpu_samples`](Self::cpu_samples) consecutive samples.
    pub cpu_threshold: f32,
    pub cpu_samples: usize,
    /// Memory must grow by at least this ratio, without ever falling back, over
    /// [`growth_samples`](Self::growth_samples) samples.
    pub growth_ratio: f32,
    pub growth_samples: usize,
    /// How far back events are considered.
    pub event_window: Duration,
}

impl Default for WarningRules {
    fn default() -> Self {
        Self {
            restart_threshold: 3,
            restart_window: Duration::from_secs(60),
            // 90% of one core: a busy dev server is normal, a pegged one is not.
            cpu_threshold: 90.0,
            cpu_samples: 15,
            growth_ratio: 1.5,
            growth_samples: 120,
            event_window: Duration::from_secs(300),
        }
    }
}

/// Everything the rules read. Borrowed, so evaluating costs no clones.
pub struct WarningInput<'a> {
    pub services: &'a [Service],
    /// Per-service resource history, oldest first.
    pub history: &'a BTreeMap<ServiceId, Vec<ResourceSample>>,
    /// Recent events, any order.
    pub events: &'a [RunscapeEvent],
}

/// What changed between two evaluations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WarningDelta {
    /// Warnings that were not active before.
    pub added: Vec<Warning>,
    /// Warnings that stopped being true, by id.
    pub removed: Vec<String>,
    /// Every currently active warning, newest activity first.
    pub current: Vec<Warning>,
}

impl WarningDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Evaluates the rules and remembers which warnings are already active.
#[derive(Debug, Default)]
pub struct WarningEngine {
    rules: WarningRules,
    active: BTreeMap<String, Warning>,
}

impl WarningEngine {
    pub fn new(rules: WarningRules) -> Self {
        Self {
            rules,
            active: BTreeMap::new(),
        }
    }

    pub fn rules(&self) -> &WarningRules {
        &self.rules
    }

    /// Run every rule. A warning that fires again keeps its original
    /// `first_seen`: "since when" is the useful half of a warning.
    pub fn evaluate(&mut self, input: WarningInput<'_>, at: SystemTime) -> WarningDelta {
        let mut fired: Vec<Warning> = Vec::new();
        fired.extend(self.restart_loops(&input, at));
        fired.extend(self.cpu_spikes(&input, at));
        fired.extend(self.memory_growth(&input, at));
        fired.extend(self.health_failures(&input, at));
        fired.extend(self.port_conflicts(&input, at));

        let mut delta = WarningDelta::default();
        let fired_ids: BTreeSet<String> = fired.iter().map(|w| w.id.clone()).collect();

        for mut warning in fired {
            match self.active.get(&warning.id) {
                Some(existing) => {
                    warning.first_seen = existing.first_seen;
                }
                None => delta.added.push(warning.clone()),
            }
            self.active.insert(warning.id.clone(), warning);
        }

        self.active.retain(|id, _| {
            let still_true = fired_ids.contains(id);
            if !still_true {
                delta.removed.push(id.clone());
            }
            still_true
        });

        delta.current = self.active.values().cloned().collect();
        delta
            .current
            .sort_by_key(|warning| std::cmp::Reverse(warning.last_seen));
        delta
    }

    /// Currently active warnings.
    pub fn active(&self) -> impl Iterator<Item = &Warning> {
        self.active.values()
    }

    /// A service that restarts repeatedly is not being restarted by a
    /// developer; it is failing to stay up.
    fn restart_loops(&self, input: &WarningInput<'_>, at: SystemTime) -> Vec<Warning> {
        let mut by_service: BTreeMap<&ServiceId, Vec<&RunscapeEvent>> = BTreeMap::new();
        for event in self.events_within(input, at, self.rules.restart_window) {
            if let EventKind::ServiceRestarted { service_id, .. } = &event.kind {
                by_service.entry(service_id).or_default().push(event);
            }
        }

        by_service
            .into_iter()
            .filter(|(_, events)| events.len() >= self.rules.restart_threshold)
            .filter_map(|(service_id, events)| {
                let service = find(input.services, service_id)?;
                Some(Warning {
                    id: warning_id("restart_loop", service_id.as_str()),
                    rule: "restart_loop".to_string(),
                    severity: Severity::Critical,
                    project_id: service.project_id.clone(),
                    service_id: Some(service_id.clone()),
                    message: format!(
                        "{} restarted {} times in the last {}s",
                        service.name,
                        events.len(),
                        self.rules.restart_window.as_secs()
                    ),
                    first_seen: at,
                    last_seen: at,
                    related_events: ids(&events),
                })
            })
            .collect()
    }

    /// Sustained CPU, not a spike in the colloquial sense: one busy second is
    /// how software works, a minute of it is a question worth asking.
    fn cpu_spikes(&self, input: &WarningInput<'_>, at: SystemTime) -> Vec<Warning> {
        input
            .services
            .iter()
            .filter(|service| service.is_running())
            .filter_map(|service| {
                let samples = input.history.get(&service.id)?;
                let recent = tail(samples, self.rules.cpu_samples)?;
                let sustained = recent
                    .iter()
                    .all(|sample| sample.cpu_percent >= self.rules.cpu_threshold);
                if !sustained {
                    return None;
                }
                let peak = recent
                    .iter()
                    .map(|s| s.cpu_percent)
                    .fold(f32::MIN, f32::max);

                Some(Warning {
                    id: warning_id("cpu_spike", service.id.as_str()),
                    rule: "cpu_spike".to_string(),
                    severity: Severity::Info,
                    project_id: service.project_id.clone(),
                    service_id: Some(service.id.clone()),
                    message: format!(
                        "{} has held above {:.0}% CPU for {} samples (peak {:.0}%)",
                        service.name, self.rules.cpu_threshold, self.rules.cpu_samples, peak
                    ),
                    first_seen: at,
                    last_seen: at,
                    related_events: Vec::new(),
                })
            })
            .collect()
    }

    /// Memory that only ever goes up. A garbage-collected runtime that never
    /// gives anything back over a two-minute window is worth a look; anything
    /// that falls back once does not fire this rule.
    fn memory_growth(&self, input: &WarningInput<'_>, at: SystemTime) -> Vec<Warning> {
        input
            .services
            .iter()
            .filter(|service| service.is_running())
            .filter_map(|service| {
                let samples = input.history.get(&service.id)?;
                let recent = tail(samples, self.rules.growth_samples)?;

                let first = recent.first()?.memory_bytes;
                let last = recent.last()?.memory_bytes;
                if first == 0 {
                    return None;
                }
                let monotonic = recent
                    .windows(2)
                    .all(|w| w[1].memory_bytes >= w[0].memory_bytes);
                let ratio = last as f32 / first as f32;
                if !monotonic || ratio < self.rules.growth_ratio {
                    return None;
                }

                Some(Warning {
                    id: warning_id("memory_growth", service.id.as_str()),
                    rule: "memory_growth".to_string(),
                    severity: Severity::Warning,
                    project_id: service.project_id.clone(),
                    service_id: Some(service.id.clone()),
                    message: format!(
                        "{} memory grew from {} to {} without falling back",
                        service.name,
                        human_bytes(first),
                        human_bytes(last)
                    ),
                    first_seen: at,
                    last_seen: at,
                    related_events: Vec::new(),
                })
            })
            .collect()
    }

    /// A service the registry currently considers unhealthy.
    fn health_failures(&self, input: &WarningInput<'_>, at: SystemTime) -> Vec<Warning> {
        let recent = self.events_within(input, at, self.rules.event_window);

        input
            .services
            .iter()
            .filter(|service| service.health == Health::Degraded)
            .map(|service| {
                let related: Vec<&RunscapeEvent> = recent
                    .iter()
                    .copied()
                    .filter(|event| {
                        matches!(
                            &event.kind,
                            EventKind::HealthChanged { service_id, .. } if service_id == &service.id
                        )
                    })
                    .collect();

                Warning {
                    id: warning_id("health_failure", service.id.as_str()),
                    rule: "health_failure".to_string(),
                    severity: Severity::Warning,
                    project_id: service.project_id.clone(),
                    service_id: Some(service.id.clone()),
                    message: format!("{} is degraded", service.name),
                    first_seen: at,
                    last_seen: at,
                    related_events: ids(&related),
                }
            })
            .collect()
    }

    /// Two live services listening on one port. On a developer machine this is
    /// usually the same app started twice, and it explains a connection going
    /// to the wrong process.
    fn port_conflicts(&self, input: &WarningInput<'_>, at: SystemTime) -> Vec<Warning> {
        let mut by_port: BTreeMap<u16, Vec<&Service>> = BTreeMap::new();
        for service in input.services.iter().filter(|s| s.is_running()) {
            let mut ports: Vec<u16> = service.endpoints.iter().map(|e| e.port).collect();
            ports.sort_unstable();
            ports.dedup();
            for port in ports {
                by_port.entry(port).or_default().push(service);
            }
        }

        by_port
            .into_iter()
            .filter(|(_, services)| services.len() > 1)
            .map(|(port, services)| {
                let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
                let project = services
                    .iter()
                    .find_map(|s| s.project_id.clone())
                    .map(Some)
                    .unwrap_or(None::<ProjectId>);

                Warning {
                    id: warning_id("port_conflict", &port.to_string()),
                    rule: "port_conflict".to_string(),
                    severity: Severity::Warning,
                    project_id: project,
                    // A conflict is about the port, not about one of its
                    // claimants, so it names none of them as *the* service.
                    service_id: None,
                    message: format!("port {port} is claimed by {}", names.join(" and ")),
                    first_seen: at,
                    last_seen: at,
                    related_events: Vec::new(),
                }
            })
            .collect()
    }

    fn events_within<'a>(
        &self,
        input: &WarningInput<'a>,
        at: SystemTime,
        window: Duration,
    ) -> Vec<&'a RunscapeEvent> {
        let cutoff = at.checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);
        input
            .events
            .iter()
            .filter(|event| event.at >= cutoff && event.at <= at)
            .collect()
    }
}

fn find<'a>(services: &'a [Service], id: &ServiceId) -> Option<&'a Service> {
    services.iter().find(|service| &service.id == id)
}

fn ids(events: &[&RunscapeEvent]) -> Vec<EventId> {
    events.iter().map(|event| event.id.clone()).collect()
}

/// The last `count` samples, or `None` when there are not that many yet. A rule
/// that fires on two samples out of a required fifteen is a false alarm.
fn tail<T>(values: &[T], count: usize) -> Option<&[T]> {
    (values.len() >= count).then(|| &values[values.len() - count..])
}

/// Stable per (rule, subject) so a firing rule updates one row.
fn warning_id(rule: &str, subject: &str) -> String {
    format!("warn_{rule}_{subject}")
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];
    for (unit, size) in UNITS {
        if bytes >= size {
            return format!("{:.0}{unit}", bytes as f64 / size as f64);
        }
    }
    "0B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runscape_core::identity::Runtime;
    use runscape_core::model::{Endpoint, Protocol, ServiceKind};
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
    }

    fn service(name: &str, port: Option<u16>) -> Service {
        Service {
            id: ServiceId::derived(name),
            project_id: Some(ProjectId::derived("/tmp/app")),
            name: name.to_string(),
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Node,
            fingerprint: format!("host|{name}"),
            health: Health::Healthy,
            instances: vec![runscape_core::model::ProcessInstance {
                pid: 100,
                parent_pid: None,
                executable: None,
                command: Vec::new(),
                cwd: None,
                started_at_epoch_secs: 1_700_000_000,
                cpu_percent: 0.0,
                memory_bytes: 0,
                virtual_memory_bytes: 0,
                thread_count: None,
                disk_read_bytes: 0,
                disk_write_bytes: 0,
            }],
            endpoints: port
                .map(|port| {
                    vec![Endpoint {
                        address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                        port,
                        protocol: Protocol::Tcp,
                        pid: Some(100),
                    }]
                })
                .unwrap_or_default(),
            first_seen: at(0),
            last_seen: at(100),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        }
    }

    fn restart(service: &Service, secs: u64, sequence: u32) -> RunscapeEvent {
        RunscapeEvent {
            id: EventId::new(1_700_000_000_000 + secs * 1000, sequence),
            at: at(secs),
            project_id: service.project_id.clone(),
            kind: EventKind::ServiceRestarted {
                service_id: service.id.clone(),
                old_pid: Some(1),
                new_pid: Some(2),
            },
        }
    }

    fn samples(values: &[(f32, u64)]) -> Vec<ResourceSample> {
        values
            .iter()
            .enumerate()
            .map(|(i, (cpu, memory))| ResourceSample::cpu_and_memory(at(i as u64), *cpu, *memory))
            .collect()
    }

    fn evaluate(
        engine: &mut WarningEngine,
        services: &[Service],
        history: &BTreeMap<ServiceId, Vec<ResourceSample>>,
        events: &[RunscapeEvent],
        now: SystemTime,
    ) -> WarningDelta {
        engine.evaluate(
            WarningInput {
                services,
                history,
                events,
            },
            now,
        )
    }

    #[test]
    fn three_restarts_in_a_minute_is_a_loop() {
        let web = service("web", None);
        let events = vec![
            restart(&web, 10, 1),
            restart(&web, 20, 2),
            restart(&web, 30, 3),
        ];
        let mut engine = WarningEngine::new(WarningRules::default());

        let delta = evaluate(&mut engine, &[web], &BTreeMap::new(), &events, at(40));

        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].rule, "restart_loop");
        assert_eq!(delta.added[0].severity, Severity::Critical);
        assert_eq!(delta.added[0].related_events.len(), 3);
    }

    #[test]
    fn two_restarts_is_a_developer_at_work() {
        let web = service("web", None);
        let events = vec![restart(&web, 10, 1), restart(&web, 20, 2)];
        let mut engine = WarningEngine::new(WarningRules::default());

        let delta = evaluate(&mut engine, &[web], &BTreeMap::new(), &events, at(30));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn restarts_outside_the_window_do_not_count() {
        let web = service("web", None);
        let events = vec![
            restart(&web, 10, 1),
            restart(&web, 20, 2),
            restart(&web, 30, 3),
        ];
        let mut engine = WarningEngine::new(WarningRules::default());

        // Two minutes later, all three are outside the one-minute window.
        let delta = evaluate(&mut engine, &[web], &BTreeMap::new(), &events, at(150));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn a_warning_that_clears_is_reported_as_removed() {
        let web = service("web", None);
        let events = vec![
            restart(&web, 10, 1),
            restart(&web, 20, 2),
            restart(&web, 30, 3),
        ];
        let mut engine = WarningEngine::new(WarningRules::default());

        let first = evaluate(
            &mut engine,
            std::slice::from_ref(&web),
            &BTreeMap::new(),
            &events,
            at(40),
        );
        assert_eq!(first.added.len(), 1);

        let later = evaluate(&mut engine, &[web], &BTreeMap::new(), &[], at(200));
        assert!(later.added.is_empty());
        assert_eq!(later.removed.len(), 1);
        assert!(later.current.is_empty());
    }

    #[test]
    fn a_repeated_warning_keeps_its_first_seen() {
        let web = service("web", None);
        let events = vec![
            restart(&web, 10, 1),
            restart(&web, 20, 2),
            restart(&web, 30, 3),
        ];
        let mut engine = WarningEngine::new(WarningRules::default());

        evaluate(
            &mut engine,
            std::slice::from_ref(&web),
            &BTreeMap::new(),
            &events,
            at(40),
        );
        let second = evaluate(&mut engine, &[web], &BTreeMap::new(), &events, at(50));

        assert!(second.added.is_empty(), "an active warning is not re-added");
        assert_eq!(second.current[0].first_seen, at(40));
        assert_eq!(second.current[0].last_seen, at(50));
    }

    #[test]
    fn sustained_cpu_fires_but_one_busy_sample_does_not() {
        let web = service("web", None);
        let rules = WarningRules {
            cpu_samples: 3,
            ..WarningRules::default()
        };

        let mut busy = BTreeMap::new();
        busy.insert(web.id.clone(), samples(&[(95.0, 1), (99.0, 1), (97.0, 1)]));
        let mut engine = WarningEngine::new(rules.clone());
        let delta = evaluate(&mut engine, std::slice::from_ref(&web), &busy, &[], at(10));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].rule, "cpu_spike");

        let mut mixed = BTreeMap::new();
        mixed.insert(web.id.clone(), samples(&[(95.0, 1), (2.0, 1), (97.0, 1)]));
        let mut engine = WarningEngine::new(rules);
        let delta = evaluate(&mut engine, &[web], &mixed, &[], at(10));
        assert!(delta.added.is_empty(), "CPU that drops is not sustained");
    }

    #[test]
    fn memory_that_only_grows_fires_and_memory_that_falls_back_does_not() {
        let web = service("web", None);
        let rules = WarningRules {
            growth_samples: 4,
            growth_ratio: 1.5,
            ..WarningRules::default()
        };

        let mut growing = BTreeMap::new();
        growing.insert(
            web.id.clone(),
            samples(&[(1.0, 100), (1.0, 150), (1.0, 200), (1.0, 260)]),
        );
        let mut engine = WarningEngine::new(rules.clone());
        let delta = evaluate(
            &mut engine,
            std::slice::from_ref(&web),
            &growing,
            &[],
            at(10),
        );
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].rule, "memory_growth");

        let mut sawtooth = BTreeMap::new();
        sawtooth.insert(
            web.id.clone(),
            samples(&[(1.0, 100), (1.0, 300), (1.0, 120), (1.0, 260)]),
        );
        let mut engine = WarningEngine::new(rules);
        let delta = evaluate(&mut engine, &[web], &sawtooth, &[], at(10));
        assert!(
            delta.added.is_empty(),
            "a GC that returns memory is healthy"
        );
    }

    #[test]
    fn a_degraded_service_is_a_health_failure() {
        let mut web = service("web", None);
        web.health = Health::Degraded;
        let mut engine = WarningEngine::new(WarningRules::default());

        let delta = evaluate(&mut engine, &[web], &BTreeMap::new(), &[], at(10));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].rule, "health_failure");
    }

    #[test]
    fn two_services_on_one_port_is_a_conflict() {
        let a = service("web", Some(3000));
        let mut b = service("web-again", Some(3000));
        b.instances[0].pid = 200;
        let mut engine = WarningEngine::new(WarningRules::default());

        let delta = evaluate(&mut engine, &[a, b], &BTreeMap::new(), &[], at(10));
        let conflict = delta
            .added
            .iter()
            .find(|w| w.rule == "port_conflict")
            .expect("a conflict fires");
        assert!(conflict.message.contains("3000"), "{}", conflict.message);
        assert_eq!(
            conflict.service_id, None,
            "a conflict blames no single side"
        );
    }

    #[test]
    fn one_service_per_port_is_not_a_conflict() {
        let a = service("web", Some(3000));
        let b = service("api", Some(3001));
        let mut engine = WarningEngine::new(WarningRules::default());

        let delta = evaluate(&mut engine, &[a, b], &BTreeMap::new(), &[], at(10));
        assert!(!delta.added.iter().any(|w| w.rule == "port_conflict"));
    }
}
