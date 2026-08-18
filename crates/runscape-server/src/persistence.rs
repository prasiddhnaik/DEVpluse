//! Persistence wiring (tasks T5.1, T5.2).
//!
//! [`Store`] is synchronous and holds a `Mutex<Connection>`, so it is owned by
//! one blocking thread and fed over a bounded channel. Two consequences, both
//! deliberate:
//!
//! * a slow disk can never block a snapshot tick — the daemon's job is to keep
//!   observing, and history is the part that may fall behind;
//! * when the queue is full, the write is dropped and counted rather than
//!   queued without bound (`AGENTS.md` rule 7). A dropped resource sample is a
//!   gap in a sparkline; a daemon that grows without limit is a bug.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use runscape_core::ids::{ProjectId, ServiceId};
use runscape_core::model::{Project, ResourceSample, RunscapeEvent, Service, Warning};
use runscape_storage::{RetentionPolicy, StorageError, Store};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Writes queued before new ones are dropped. One tick enqueues one message,
/// so this is several minutes of backlog at 1 Hz — far more than a working
/// disk ever needs, and still bounded.
const QUEUE_DEPTH: usize = 256;

/// How often retention runs. Deleting is cheap and idempotent; doing it every
/// few minutes keeps each pass small.
pub const RETENTION_INTERVAL: Duration = Duration::from_secs(300);

/// Everything one tick has to persist.
#[derive(Debug, Default)]
pub struct TickWrite {
    pub projects: Vec<Project>,
    /// Only the services that changed. Rewriting every service every second
    /// would be a write per service per tick for data that did not move.
    pub services: Vec<Service>,
    pub events: Vec<RunscapeEvent>,
    pub samples: Vec<(ServiceId, ResourceSample)>,
    pub warnings: Vec<Warning>,
}

impl TickWrite {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
            && self.services.is_empty()
            && self.events.is_empty()
            && self.samples.is_empty()
            && self.warnings.is_empty()
    }
}

enum Command {
    Write(Box<TickWrite>),
    Retention(SystemTime),
}

/// Handle to the writer thread. Cloning it is cheap; dropping the last one
/// stops the writer.
#[derive(Clone)]
pub struct Persistence {
    tx: mpsc::Sender<Command>,
    dropped: Arc<AtomicU64>,
    path: Option<PathBuf>,
}

impl Persistence {
    /// Start a writer against the database at `path`.
    pub fn open(path: &Path, policy: RetentionPolicy) -> Result<Self, StorageError> {
        let store = Store::open(path)?;
        info!(path = %path.display(), "persisting to sqlite");
        Ok(Self::spawn(store, policy, Some(path.to_path_buf())))
    }

    /// Start a writer against a database that vanishes on drop. Used by tests
    /// and by `--no-persistence`, so the rest of the daemon has one code path.
    pub fn in_memory(policy: RetentionPolicy) -> Result<Self, StorageError> {
        Ok(Self::spawn(Store::open_in_memory()?, policy, None))
    }

    fn spawn(store: Store, policy: RetentionPolicy, path: Option<PathBuf>) -> Self {
        let (tx, mut rx) = mpsc::channel::<Command>(QUEUE_DEPTH);
        let dropped = Arc::new(AtomicU64::new(0));

        // `spawn_blocking`, not `spawn`: every call on `Store` blocks on a
        // mutex and on the disk, which must not happen on a runtime worker.
        tokio::task::spawn_blocking(move || {
            while let Some(command) = rx.blocking_recv() {
                match command {
                    Command::Write(write) => apply(&store, &write),
                    Command::Retention(now) => match store.apply_retention(&policy, now) {
                        Ok(report) => debug!(
                            events_deleted = report.events_deleted,
                            samples_deleted = report.samples_deleted,
                            "retention pass"
                        ),
                        Err(error) => warn!(%error, "retention failed"),
                    },
                }
            }
        });

        Self { tx, dropped, path }
    }

    /// The file being written to, or `None` for an in-memory database.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Writes dropped because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Queue a tick. Never blocks and never fails the caller: history is the
    /// part of the daemon that is allowed to fall behind.
    pub fn write(&self, write: TickWrite) {
        if write.is_empty() {
            return;
        }
        if self.tx.try_send(Command::Write(Box::new(write))).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            warn!(
                dropped,
                "persistence queue is full; dropping a tick's history"
            );
        }
    }

    /// Queue a retention pass.
    pub fn retention(&self, now: SystemTime) {
        let _ = self.tx.try_send(Command::Retention(now));
    }
}

/// Write one tick, row group by row group.
///
/// A failure in one group does not abandon the others: a service whose project
/// row is missing (its project stopped being observed in the same tick) must
/// not cost the developer the event that explains it. Every failure is logged
/// with what it was writing.
fn apply(store: &Store, write: &TickWrite) {
    for project in &write.projects {
        if let Err(error) = store.upsert_project(project) {
            warn!(%error, project = %project.name, "storing a project failed");
        }
    }
    for service in &write.services {
        if let Err(error) = store.upsert_service(service) {
            warn!(%error, service = %service.name, "storing a service failed");
        }
    }
    for warning in &write.warnings {
        if let Err(error) = store.upsert_warning(warning) {
            warn!(%error, rule = %warning.rule, "storing a warning failed");
        }
    }
    if let Err(error) = store.record_events(&write.events) {
        warn!(%error, count = write.events.len(), "storing events failed");
    }
    if let Err(error) = store.record_resource_samples(&write.samples) {
        warn!(%error, count = write.samples.len(), "storing resource samples failed");
    }
}

/// What the daemon reads back at startup so a restart does not look like a
/// fresh machine with no history.
#[derive(Debug, Default)]
pub struct RestoredHistory {
    pub projects: BTreeMap<ProjectId, Project>,
    /// Oldest first, ready to push into the in-memory ring.
    pub events: Vec<RunscapeEvent>,
    pub warnings: Vec<Warning>,
}

/// Read the recent past out of `path`.
///
/// Services are deliberately not restored: a persisted service carries PIDs
/// and ports that were true before the daemon stopped, and a PID is a lie the
/// moment the process exits (`AGENTS.md` rule 5). The registry rebuilds them
/// from observation within one tick.
pub fn restore(path: &Path, event_limit: usize) -> Result<RestoredHistory, StorageError> {
    let store = Store::open(path)?;
    let mut events = store.recent_events(None, event_limit)?;
    // The store returns newest first; the ring wants oldest first.
    events.reverse();

    Ok(RestoredHistory {
        projects: store
            .projects()?
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect(),
        events,
        warnings: store.warnings(None)?,
    })
}

/// Default database location: `~/.runscape/runscape.db`.
///
/// One visible directory the developer can delete, rather than three
/// platform-specific ones they have to look up. `None` when the home directory
/// is not knowable, in which case the daemon runs without persistence and says
/// so.
pub fn default_database_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".runscape").join("runscape.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runscape_core::ids::EventId;
    use runscape_core::model::EventKind;

    fn event(secs: u64) -> RunscapeEvent {
        RunscapeEvent {
            id: EventId::new(1_700_000_000_000 + secs * 1000, secs as u32),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs),
            project_id: None,
            kind: EventKind::ServiceStarted {
                service_id: ServiceId::derived("web"),
                pid: Some(1),
            },
        }
    }

    #[tokio::test]
    async fn an_empty_write_is_not_queued() {
        let persistence = Persistence::in_memory(RetentionPolicy::default()).expect("opens");
        persistence.write(TickWrite::default());
        assert_eq!(persistence.dropped(), 0);
    }

    #[tokio::test]
    async fn a_full_queue_drops_rather_than_grows() {
        // A writer that is never polled: the channel fills and stays full.
        let (tx, _rx) = mpsc::channel::<Command>(1);
        let persistence = Persistence {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            path: None,
        };

        for _ in 0..10 {
            persistence.write(TickWrite {
                events: vec![event(1)],
                ..TickWrite::default()
            });
        }

        assert!(
            persistence.dropped() >= 8,
            "a full queue must drop, not buffer: {}",
            persistence.dropped()
        );
    }

    #[test]
    fn the_default_path_lives_under_home() {
        let path = default_database_path().expect("HOME is set in the test environment");
        assert!(path.ends_with(".runscape/runscape.db"), "{path:?}");
    }
}
