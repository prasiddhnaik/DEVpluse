//! Temporal correlation (tasks T7.2, T7.4).
//!
//! "You saved a file, the service restarted two seconds later, its health went
//! bad five seconds after that" is the sentence a developer actually needs. It
//! is also, strictly, three timestamps and nothing more.
//!
//! So this module states adjacency and never claims cause (`DECISIONS.md`
//! D008): everything it produces is "these happened close together, in this
//! order, to the same subject". The relation it reports is spelled out per
//! event, so a developer can judge it themselves.

use std::time::{Duration, SystemTime};

use runscape_core::ids::{EventId, ProjectId, ServiceId};
use runscape_core::model::{EventKind, RunscapeEvent};
use serde::{Deserialize, Serialize};

/// How far either side of an event to look for related ones.
pub const CONTEXT_WINDOW: Duration = Duration::from_secs(30);

/// How soon after a file change a restart is worth reporting as adjacent. A
/// dev server that reloads takes a second or two; a restart a minute later is
/// somebody typing `^C`.
pub const RESTART_ADJACENCY: Duration = Duration::from_secs(10);

/// Why an event is in another event's context. Never "caused by".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Same service.
    SameService,
    /// Same project, different service.
    SameProject,
    /// A file changed in this project shortly before the anchor event. This is
    /// the closest thing Runscape has to an explanation, and it is still only
    /// an ordering.
    PrecedingFileChange,
    /// Close in time, nothing else in common.
    Temporal,
}

/// One event in another's context.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedEvent {
    pub event: RunscapeEvent,
    pub relation: Relation,
    /// Signed offset from the anchor: negative is before, positive is after.
    pub offset_ms: i64,
}

/// The anchor plus what happened around it.
#[derive(Debug, Clone, PartialEq)]
pub struct EventContext {
    pub anchor: RunscapeEvent,
    /// Newest first, so the event just before the anchor comes first.
    pub before: Vec<RelatedEvent>,
    /// Oldest first, so the event just after the anchor comes first.
    pub after: Vec<RelatedEvent>,
}

/// Build the context of `anchor` out of `events` (task T7.4).
///
/// `events` may be in any order and may contain the anchor itself, which is
/// excluded from its own context.
pub fn context(events: &[RunscapeEvent], anchor: &RunscapeEvent, window: Duration) -> EventContext {
    let mut before = Vec::new();
    let mut after = Vec::new();

    for event in events {
        if event.id == anchor.id {
            continue;
        }
        let Some(offset_ms) = offset_within(anchor.at, event.at, window) else {
            continue;
        };

        let related = RelatedEvent {
            event: event.clone(),
            relation: relation_between(anchor, event, offset_ms),
            offset_ms,
        };
        if offset_ms < 0 {
            before.push(related);
        } else {
            after.push(related);
        }
    }

    // Nearest to the anchor first on both sides: proximity is the whole signal.
    before.sort_by_key(|related| -related.offset_ms);
    after.sort_by_key(|related| related.offset_ms);

    EventContext {
        anchor: anchor.clone(),
        before,
        after,
    }
}

/// The file change, if any, that immediately preceded `restart` in the same
/// project (task T7.2).
///
/// Returns the *latest* such change: when a save touches several files, the one
/// closest to the restart is the one worth showing.
pub fn preceding_file_change<'a>(
    events: &'a [RunscapeEvent],
    restart: &RunscapeEvent,
    within: Duration,
) -> Option<&'a RunscapeEvent> {
    let project = restart.project_id.as_ref()?;

    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::FileChanged { .. }))
        .filter(|event| event.project_id.as_ref() == Some(project))
        .filter(|event| {
            restart
                .at
                .duration_since(event.at)
                .is_ok_and(|gap| gap <= within && !gap.is_zero())
        })
        .max_by_key(|event| event.at)
}

/// Signed millisecond offset of `other` from `anchor`, or `None` when it falls
/// outside `window`.
fn offset_within(anchor: SystemTime, other: SystemTime, window: Duration) -> Option<i64> {
    match other.duration_since(anchor) {
        Ok(after) if after <= window => Some(after.as_millis() as i64),
        Ok(_) => None,
        Err(error) => {
            let before = error.duration();
            (before <= window).then(|| -(before.as_millis() as i64))
        }
    }
}

fn relation_between(anchor: &RunscapeEvent, other: &RunscapeEvent, offset_ms: i64) -> Relation {
    let same_project = matches!(
        (project_of(anchor), project_of(other)),
        (Some(a), Some(b)) if a == b
    );

    if same_project
        && offset_ms < 0
        && matches!(other.kind, EventKind::FileChanged { .. })
        && (-offset_ms) as u128 <= RESTART_ADJACENCY.as_millis()
    {
        return Relation::PrecedingFileChange;
    }

    match (service_of(anchor), service_of(other)) {
        (Some(a), Some(b)) if a == b => Relation::SameService,
        _ if same_project => Relation::SameProject,
        _ => Relation::Temporal,
    }
}

fn project_of(event: &RunscapeEvent) -> Option<&ProjectId> {
    match &event.kind {
        EventKind::ProjectDetected { project_id } | EventKind::FileChanged { project_id, .. } => {
            Some(project_id)
        }
        _ => event.project_id.as_ref(),
    }
}

/// The service an event is about, if it is about one.
pub fn service_of(event: &RunscapeEvent) -> Option<&ServiceId> {
    match &event.kind {
        EventKind::ServiceStarted { service_id, .. }
        | EventKind::ServiceStopped { service_id, .. }
        | EventKind::ServiceRestarted { service_id, .. }
        | EventKind::HealthChanged { service_id, .. }
        | EventKind::ResourceWarning { service_id, .. } => Some(service_id),
        EventKind::PortOpened { service_id, .. } | EventKind::PortClosed { service_id, .. } => {
            service_id.as_ref()
        }
        EventKind::ConnectionStarted { source, .. } => Some(source),
        EventKind::ProjectDetected { .. }
        | EventKind::ConnectionEnded { .. }
        | EventKind::FileChanged { .. } => None,
    }
}

/// Every event id in a context, anchor excluded. Convenient for storing a
/// correlation alongside a warning.
pub fn context_ids(context: &EventContext) -> Vec<EventId> {
    context
        .before
        .iter()
        .chain(context.after.iter())
        .map(|related| related.event.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runscape_core::model::Health;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
    }

    fn project() -> ProjectId {
        ProjectId::derived("/tmp/app")
    }

    fn event(secs: u64, sequence: u32, kind: EventKind) -> RunscapeEvent {
        RunscapeEvent {
            id: EventId::new(1_700_000_000_000 + secs * 1_000, sequence),
            at: at(secs),
            project_id: Some(project()),
            kind,
        }
    }

    fn file_changed(secs: u64, sequence: u32) -> RunscapeEvent {
        event(
            secs,
            sequence,
            EventKind::FileChanged {
                project_id: project(),
                path: PathBuf::from("/tmp/app/src/main.rs"),
            },
        )
    }

    fn restarted(secs: u64, sequence: u32) -> RunscapeEvent {
        event(
            secs,
            sequence,
            EventKind::ServiceRestarted {
                service_id: ServiceId::derived("web"),
                old_pid: Some(1),
                new_pid: Some(2),
            },
        )
    }

    fn health_failed(secs: u64, sequence: u32) -> RunscapeEvent {
        event(
            secs,
            sequence,
            EventKind::HealthChanged {
                service_id: ServiceId::derived("web"),
                from: Health::Healthy,
                to: Health::Degraded,
            },
        )
    }

    #[test]
    fn the_documented_story_reads_in_order() {
        // TASKS.md T7.2: file changed, service restarted 2s later, health
        // failed 5s after that.
        let events = vec![file_changed(0, 1), restarted(2, 2), health_failed(7, 3)];
        let context = context(&events, &events[1], CONTEXT_WINDOW);

        assert_eq!(context.before.len(), 1);
        assert_eq!(context.before[0].relation, Relation::PrecedingFileChange);
        assert_eq!(context.before[0].offset_ms, -2_000);

        assert_eq!(context.after.len(), 1);
        assert_eq!(context.after[0].relation, Relation::SameService);
        assert_eq!(context.after[0].offset_ms, 5_000);
    }

    #[test]
    fn events_outside_the_window_are_not_context() {
        let events = vec![file_changed(0, 1), restarted(2, 2), health_failed(600, 3)];
        let context = context(&events, &events[1], CONTEXT_WINDOW);

        assert!(context.after.is_empty(), "ten minutes later is not context");
    }

    #[test]
    fn an_event_is_not_its_own_context() {
        let events = vec![restarted(2, 2)];
        let context = context(&events, &events[0], CONTEXT_WINDOW);
        assert!(context.before.is_empty() && context.after.is_empty());
    }

    #[test]
    fn the_nearest_preceding_save_is_the_one_reported() {
        let events = vec![file_changed(0, 1), file_changed(4, 2), restarted(5, 3)];
        let found = preceding_file_change(&events, &events[2], RESTART_ADJACENCY)
            .expect("a change precedes the restart");
        assert_eq!(found.at, at(4));
    }

    #[test]
    fn a_save_long_before_a_restart_is_not_reported_as_adjacent() {
        let events = vec![file_changed(0, 1), restarted(60, 2)];
        assert!(preceding_file_change(&events, &events[1], RESTART_ADJACENCY).is_none());
    }

    #[test]
    fn a_save_in_another_project_is_not_adjacent() {
        let other_project = ProjectId::derived("/tmp/other");
        let mut change = file_changed(0, 1);
        change.project_id = Some(other_project.clone());
        change.kind = EventKind::FileChanged {
            project_id: other_project,
            path: PathBuf::from("/tmp/other/src/main.rs"),
        };

        let events = vec![change, restarted(2, 2)];
        assert!(preceding_file_change(&events, &events[1], RESTART_ADJACENCY).is_none());
    }

    #[test]
    fn context_ids_are_everything_but_the_anchor() {
        let events = vec![file_changed(0, 1), restarted(2, 2), health_failed(7, 3)];
        let context = context(&events, &events[1], CONTEXT_WINDOW);
        assert_eq!(context_ids(&context).len(), 2);
    }
}
