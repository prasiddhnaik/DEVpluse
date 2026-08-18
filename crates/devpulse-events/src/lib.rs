//! DevPulse event derivation.
//!
//! Consumes the deltas produced by the registry ([`RegistryDelta`]) and the
//! [`TopologyBuilder`] and turns them into durable [`DevPulseEvent`]s.

pub mod correlation;
pub mod warnings;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devpulse_core::ids::{EventId, ProjectId, ServiceId};
use devpulse_core::model::{DevPulseEvent, EventKind};
use devpulse_core::registry::RegistryDelta;
use devpulse_core::topology::TopologyDelta;

/// A service that stops and returns within this window is a restart.
pub const RESTART_DEBOUNCE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct EventDeriver {
    sequence: u32,
    pending_stop: BTreeMap<ServiceId, StopInfo>,
    announced_projects: std::collections::BTreeSet<ProjectId>,
}

#[derive(Debug, Clone)]
struct StopInfo {
    at: SystemTime,
    /// `None` for a container, which has no host PID.
    pid: Option<u32>,
}

impl EventDeriver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn derive(
        &mut self,
        delta: &RegistryDelta,
        topology: &TopologyDelta,
        at: SystemTime,
    ) -> Vec<DevPulseEvent> {
        let mut events = Vec::new();
        let projects = &delta.projects;

        // Record stops for possible debounce resolution.
        for (service_id, pid) in &delta.stopped {
            self.pending_stop
                .insert(service_id.clone(), StopInfo { at, pid: *pid });
        }

        let mut resolved_by_this_tick: Vec<ServiceId> = Vec::new();

        // START: might resolve a pending stop as a restart.
        for (service_id, pid) in &delta.started {
            let project = projects.get(service_id).and_then(|p| p.clone());
            let was_pending = self.pending_stop.remove(service_id);

            match was_pending {
                Some(stopped)
                    if at.duration_since(stopped.at).unwrap_or_default()
                        <= RESTART_DEBOUNCE_WINDOW =>
                {
                    // Debounce window: treat as restart.
                    events.push(DevPulseEvent {
                        id: self.next_id(at),
                        at,
                        project_id: project,
                        kind: EventKind::ServiceRestarted {
                            service_id: service_id.clone(),
                            old_pid: stopped.pid,
                            new_pid: *pid,
                        },
                    });
                    resolved_by_this_tick.push(service_id.clone());
                }
                Some(stopped) => {
                    // Too old: emit stop, then start.
                    events.push(DevPulseEvent {
                        id: self.next_id(at),
                        at,
                        project_id: projects.get(service_id).and_then(|p| p.clone()),
                        kind: EventKind::ServiceStopped {
                            service_id: service_id.clone(),
                            pid: stopped.pid,
                        },
                    });
                    events.push(DevPulseEvent {
                        id: self.next_id(at),
                        at,
                        project_id: project,
                        kind: EventKind::ServiceStarted {
                            service_id: service_id.clone(),
                            pid: *pid,
                        },
                    });
                }
                None => {
                    // No pending stop: plain start.
                    events.push(DevPulseEvent {
                        id: self.next_id(at),
                        at,
                        project_id: project,
                        kind: EventKind::ServiceStarted {
                            service_id: service_id.clone(),
                            pid: *pid,
                        },
                    });
                }
            }
        }

        // In-place restarts (registry collapsed PID change).
        for (service_id, old_pid, new_pid) in &delta.restarted {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: projects.get(service_id).and_then(|p| p.clone()),
                kind: EventKind::ServiceRestarted {
                    service_id: service_id.clone(),
                    old_pid: *old_pid,
                    new_pid: *new_pid,
                },
            });
        }

        // Flush stale pending stops (older than debounce window) that weren't resolved.
        for (id, stopped) in std::mem::take(&mut self.pending_stop) {
            if resolved_by_this_tick.contains(&id) {
                continue;
            }
            if at.duration_since(stopped.at).unwrap_or_default() > RESTART_DEBOUNCE_WINDOW {
                events.push(DevPulseEvent {
                    id: self.next_id(at),
                    at,
                    project_id: projects.get(&id).and_then(|p| p.clone()),
                    kind: EventKind::ServiceStopped {
                        service_id: id,
                        pid: stopped.pid,
                    },
                });
            } else {
                // Still within debounce window; keep it pending.
                self.pending_stop.insert(id, stopped);
            }
        }

        // Ports and health changes.
        for (service_id, port) in &delta.ports_opened {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: projects.get(service_id).and_then(|p| p.clone()),
                kind: EventKind::PortOpened {
                    service_id: Some(service_id.clone()),
                    port: *port,
                },
            });
        }
        for (service_id, port) in &delta.ports_closed {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: projects.get(service_id).and_then(|p| p.clone()),
                kind: EventKind::PortClosed {
                    service_id: Some(service_id.clone()),
                    port: *port,
                },
            });
        }

        for (service_id, from, to) in &delta.health_changed {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: projects.get(service_id).and_then(|p| p.clone()),
                kind: EventKind::HealthChanged {
                    service_id: service_id.clone(),
                    from: *from,
                    to: *to,
                },
            });
        }

        // Connection events.
        for connection in &topology.added {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: None,
                kind: EventKind::ConnectionStarted {
                    connection_id: connection.id.clone(),
                    source: connection.source.clone(),
                    target: connection.target.clone(),
                    target_port: connection.target_port,
                },
            });
        }
        for id in &topology.removed {
            events.push(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: None,
                kind: EventKind::ConnectionEnded {
                    connection_id: id.clone(),
                },
            });
        }

        events
    }

    pub fn announce_project(
        &mut self,
        project_id: &ProjectId,
        at: SystemTime,
    ) -> Option<DevPulseEvent> {
        if self.announced_projects.insert(project_id.clone()) {
            Some(DevPulseEvent {
                id: self.next_id(at),
                at,
                project_id: Some(project_id.clone()),
                kind: EventKind::ProjectDetected {
                    project_id: project_id.clone(),
                },
            })
        } else {
            None
        }
    }

    /// A file changed under a watched project root (task T7.1).
    ///
    /// The deriver mints it so that every event in the timeline comes from one
    /// id sequence and orders correctly against the events around it.
    pub fn file_changed(
        &mut self,
        project_id: &ProjectId,
        path: std::path::PathBuf,
        at: SystemTime,
    ) -> DevPulseEvent {
        DevPulseEvent {
            id: self.next_id(at),
            at,
            project_id: Some(project_id.clone()),
            kind: EventKind::FileChanged {
                project_id: project_id.clone(),
                path,
            },
        }
    }

    fn next_id(&mut self, at: SystemTime) -> EventId {
        self.sequence = self.sequence.wrapping_add(1);
        EventId::new(millis(at), self.sequence)
    }
}

fn millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
    }

    fn sid() -> ServiceId {
        ServiceId::derived("web")
    }

    #[test]
    fn a_fast_stop_and_start_is_one_restart() {
        let mut deriver = EventDeriver::new();
        let id = sid();

        let stop = RegistryDelta {
            stopped: vec![(id.clone(), Some(100))],
            ..RegistryDelta::default()
        };
        deriver.derive(&stop, &TopologyDelta::default(), at(0));

        let start = RegistryDelta {
            started: vec![(id.clone(), Some(101))],
            ..RegistryDelta::default()
        };
        let events = deriver.derive(&start, &TopologyDelta::default(), at(1));

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].kind,
            EventKind::ServiceRestarted {
                old_pid: Some(100),
                new_pid: Some(101),
                ..
            }
        ));
    }

    #[test]
    fn a_slow_stop_and_start_is_stop_then_start() {
        let mut deriver = EventDeriver::new();
        let id = sid();

        let stop = RegistryDelta {
            stopped: vec![(id.clone(), Some(100))],
            ..RegistryDelta::default()
        };

        let mut events = Vec::new();
        events.extend(deriver.derive(&stop, &TopologyDelta::default(), at(0)));
        events.extend(deriver.derive(&RegistryDelta::default(), &TopologyDelta::default(), at(10)));

        let start = RegistryDelta {
            started: vec![(id.clone(), Some(101))],
            ..RegistryDelta::default()
        };
        events.extend(deriver.derive(&start, &TopologyDelta::default(), at(11)));

        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0].kind,
            EventKind::ServiceStopped { pid: Some(100), .. }
        ));
        assert!(matches!(
            &events[1].kind,
            EventKind::ServiceStarted { pid: Some(101), .. }
        ));
    }

    #[test]
    fn in_place_restart_is_reported_directly() {
        let mut deriver = EventDeriver::new();
        let id = sid();
        let delta = RegistryDelta {
            restarted: vec![(id.clone(), Some(200), Some(201))],
            ..RegistryDelta::default()
        };

        let events = deriver.derive(&delta, &TopologyDelta::default(), at(0));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].kind,
            EventKind::ServiceRestarted {
                old_pid: Some(200),
                new_pid: Some(201),
                ..
            }
        ));
    }
}
