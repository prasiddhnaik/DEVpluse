//! The persistence handle.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use devpulse_core::model::{
    DevPulseEvent, EventKind, Health, Project, ResourceSample, Service, ServiceKind, Severity,
    Warning,
};
use devpulse_core::{EventId, ProjectId, ServiceId};
use rusqlite::{Connection, OptionalExtension, Params, Row, Statement, params};

use crate::codec::{self, bytes_from_sql, bytes_to_sql, count_to_sql, from_millis, to_millis};
use crate::error::StorageError;
use crate::retention::{self, RetentionPolicy, RetentionReport};
use crate::schema;

/// Local persistence for projects, services, events, warnings, aliases and a
/// bounded slice of resource history.
///
/// # Concurrency
///
/// Every method takes `&self` and serialises on an internal mutex, so the
/// daemon can share one `Store` across tasks without an `Arc<Mutex<_>>` at every
/// call site — SQLite would serialise the writes anyway, and a single
/// connection makes WAL checkpointing and transaction scoping obvious.
///
/// A poisoned mutex is recovered rather than propagated: a panic elsewhere
/// leaves SQLite consistent (an in-flight `Transaction` rolls back when its
/// guard drops), so refusing every later write would turn one bug into total
/// data loss.
///
/// # Blocking
///
/// These calls are synchronous file I/O. Async callers must wrap them in
/// `tokio::task::spawn_blocking`; running them on a runtime worker would stall
/// the collectors (`AGENTS.md` rule 7).
///
/// # Fidelity
///
/// Reads reconstruct the domain value that was written, with two documented
/// exceptions: timestamps are truncated to milliseconds, and paths round-trip
/// through lossy UTF-8 (which is already true of `ProjectId`, derived upstream
/// from `Path::to_string_lossy`). Nothing is invented — a column that was never
/// written reads back as `None`, never as a plausible-looking default.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

// Read queries are spelled out rather than assembled at call time: a `format!`
// per read would allocate, and the two variants of each query differ only by a
// `WHERE` clause that has to stay index-shaped. `?1 IS NULL OR project_id = ?1`
// would collapse them into one string at the cost of the index.
const SELECT_PROJECTS: &str = "SELECT id, root, name, kind, confidence, evidence, first_seen, \
                               last_seen FROM projects ORDER BY name ASC, id ASC";

const SELECT_SERVICES: &str = "SELECT id, project_id, name, kind, runtime, fingerprint, health, \
                               instances, endpoints, first_seen, last_seen, restart_count \
                               FROM services ORDER BY name ASC, id ASC";
const SELECT_SERVICES_IN_PROJECT: &str = "SELECT id, project_id, name, kind, runtime, fingerprint, health, instances, endpoints, \
     first_seen, last_seen, restart_count FROM services \
     WHERE project_id = ?1 ORDER BY name ASC, id ASC";

const SELECT_EVENTS: &str =
    "SELECT id, at, project_id, kind FROM events ORDER BY at DESC, id DESC LIMIT ?1";
const SELECT_EVENTS_IN_PROJECT: &str = "SELECT id, at, project_id, kind FROM events \
                                        WHERE project_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2";

const SELECT_WARNINGS: &str = "SELECT id, rule, severity, project_id, service_id, message, \
                               first_seen, last_seen, related_events FROM warnings \
                               ORDER BY last_seen DESC, id ASC";
const SELECT_WARNINGS_IN_PROJECT: &str = "SELECT id, rule, severity, project_id, service_id, message, first_seen, last_seen, \
     related_events FROM warnings WHERE project_id = ?1 ORDER BY last_seen DESC, id ASC";

impl Store {
    /// Open (creating if needed) the database at `path`, applying the schema.
    ///
    /// Missing parent directories are created: the daemon's data directory not
    /// existing yet on first run is normal, not an error.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StorageError::Directory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Self::from_connection(Connection::open(path)?, true)
    }

    /// An anonymous database that vanishes on drop. For tests and for a daemon
    /// asked to keep no history.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::from_connection(Connection::open_in_memory()?, false)
    }

    fn from_connection(mut conn: Connection, on_disk: bool) -> Result<Self, StorageError> {
        configure(&conn, on_disk)?;
        schema::apply(&mut conn, SystemTime::now())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record a project, preserving the earliest `first_seen` and the latest
    /// `last_seen` ever observed for it.
    ///
    /// The caller may legitimately pass a fresh `first_seen` after a daemon
    /// restart (the resolver has no memory), and "when did this project first
    /// appear" would otherwise reset every morning.
    pub fn upsert_project(&self, project: &Project) -> Result<(), StorageError> {
        let root = project.root.to_string_lossy();
        self.conn().execute(
            "INSERT INTO projects (id, root, name, kind, confidence, evidence, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 root = excluded.root,
                 name = excluded.name,
                 kind = excluded.kind,
                 confidence = excluded.confidence,
                 evidence = excluded.evidence,
                 first_seen = MIN(projects.first_seen, excluded.first_seen),
                 last_seen = MAX(projects.last_seen, excluded.last_seen)",
            params![
                project.id.as_str(),
                root.as_ref(),
                project.name,
                codec::encode(&project.kind)?,
                f64::from(project.confidence),
                serde_json::to_string(&project.evidence)?,
                to_millis(project.first_seen),
                to_millis(project.last_seen),
            ],
        )?;
        Ok(())
    }

    /// Record a service, with the same `first_seen`/`last_seen` treatment as
    /// [`Store::upsert_project`].
    ///
    /// `project_id` is a foreign key, so the project must be upserted first;
    /// that ordering is what stops a typo from stranding services under a
    /// project that never existed.
    pub fn upsert_service(&self, service: &Service) -> Result<(), StorageError> {
        self.conn().execute(
            "INSERT INTO services (id, project_id, name, kind, runtime, fingerprint, health,
                                   instances, endpoints, first_seen, last_seen, restart_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                 project_id = excluded.project_id,
                 name = excluded.name,
                 kind = excluded.kind,
                 runtime = excluded.runtime,
                 fingerprint = excluded.fingerprint,
                 health = excluded.health,
                 instances = excluded.instances,
                 endpoints = excluded.endpoints,
                 first_seen = MIN(services.first_seen, excluded.first_seen),
                 last_seen = MAX(services.last_seen, excluded.last_seen),
                 restart_count = excluded.restart_count",
            params![
                service.id.as_str(),
                service.project_id.as_ref().map(ProjectId::as_str),
                service.name,
                codec::encode(&service.kind)?,
                codec::encode(&service.runtime)?,
                service.fingerprint,
                codec::encode(&service.health)?,
                serde_json::to_string(&service.instances)?,
                serde_json::to_string(&service.endpoints)?,
                to_millis(service.first_seen),
                to_millis(service.last_seen),
                service.restart_count,
            ],
        )?;
        Ok(())
    }

    /// Append events, returning how many were new.
    ///
    /// Re-recording an already stored [`EventId`] is a no-op, which makes a
    /// retried flush harmless: the daemon derives events from snapshots and must
    /// be free to retry a write it is not sure landed.
    pub fn record_events(&self, events: &[DevPulseEvent]) -> Result<usize, StorageError> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut guard = self.conn();
        let tx = guard.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO events (id, at, project_id, kind) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for event in events {
                inserted += stmt.execute(params![
                    event.id.as_str(),
                    to_millis(event.at),
                    event.project_id.as_ref().map(ProjectId::as_str),
                    codec::encode(&event.kind)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Record a warning. A rule that keeps firing updates one row rather than
    /// accumulating duplicates, because [`Warning::id`] is stable per rule and
    /// subject.
    pub fn upsert_warning(&self, warning: &Warning) -> Result<(), StorageError> {
        self.conn().execute(
            "INSERT INTO warnings (id, rule, severity, project_id, service_id, message,
                                   first_seen, last_seen, related_events)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 rule = excluded.rule,
                 severity = excluded.severity,
                 project_id = excluded.project_id,
                 service_id = excluded.service_id,
                 message = excluded.message,
                 first_seen = MIN(warnings.first_seen, excluded.first_seen),
                 last_seen = MAX(warnings.last_seen, excluded.last_seen),
                 related_events = excluded.related_events",
            params![
                warning.id,
                warning.rule,
                codec::encode(&warning.severity)?,
                warning.project_id.as_ref().map(ProjectId::as_str),
                warning.service_id.as_ref().map(ServiceId::as_str),
                warning.message,
                to_millis(warning.first_seen),
                to_millis(warning.last_seen),
                serde_json::to_string(&warning.related_events)?,
            ],
        )?;
        Ok(())
    }

    /// Name a service. Aliases are the developer's own words, so they are stored
    /// without a foreign key and survive the service disappearing.
    pub fn set_alias(&self, service: &ServiceId, alias: &str) -> Result<(), StorageError> {
        self.conn().execute(
            "INSERT INTO aliases (service_id, alias) VALUES (?1, ?2)
             ON CONFLICT(service_id) DO UPDATE SET alias = excluded.alias",
            params![service.as_str(), alias],
        )?;
        Ok(())
    }

    pub fn alias(&self, service: &ServiceId) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT alias FROM aliases WHERE service_id = ?1",
                [service.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Append one CPU/memory reading. Bounded by
    /// [`Store::apply_retention`]; nothing here trims, so a caller that never
    /// applies retention will grow the file, by its own choice.
    pub fn record_resource_sample(
        &self,
        service: &ServiceId,
        sample: &ResourceSample,
    ) -> Result<(), StorageError> {
        self.conn().execute(
            "INSERT INTO resource_samples (service_id, at, cpu_percent, memory_bytes)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                service.as_str(),
                to_millis(sample.at),
                f64::from(sample.cpu_percent),
                bytes_to_sql(sample.memory_bytes),
            ],
        )?;
        Ok(())
    }

    /// All known projects, ordered by name then id so the listing is stable
    /// across calls.
    pub fn projects(&self) -> Result<Vec<Project>, StorageError> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(SELECT_PROJECTS)?;
        collect(&mut stmt, [], project_from_row)
    }

    /// Services, optionally restricted to one project. Services with no project
    /// are excluded when a filter is given.
    pub fn services(&self, project: Option<&ProjectId>) -> Result<Vec<Service>, StorageError> {
        let conn = self.conn();
        match project {
            Some(project) => {
                let mut stmt = conn.prepare_cached(SELECT_SERVICES_IN_PROJECT)?;
                collect(&mut stmt, [project.as_str()], service_from_row)
            }
            None => {
                let mut stmt = conn.prepare_cached(SELECT_SERVICES)?;
                collect(&mut stmt, [], service_from_row)
            }
        }
    }

    /// The newest `limit` events, newest first — the order the timeline renders
    /// and the order that makes `LIMIT` mean "most recent".
    pub fn recent_events(
        &self,
        project: Option<&ProjectId>,
        limit: usize,
    ) -> Result<Vec<DevPulseEvent>, StorageError> {
        let conn = self.conn();
        let limit = count_to_sql(limit);
        match project {
            Some(project) => {
                let mut stmt = conn.prepare_cached(SELECT_EVENTS_IN_PROJECT)?;
                collect(&mut stmt, (project.as_str(), limit), event_from_row)
            }
            None => {
                let mut stmt = conn.prepare_cached(SELECT_EVENTS)?;
                collect(&mut stmt, [limit], event_from_row)
            }
        }
    }

    /// Warnings, most recently seen first, optionally restricted to one project.
    pub fn warnings(&self, project: Option<&ProjectId>) -> Result<Vec<Warning>, StorageError> {
        let conn = self.conn();
        match project {
            Some(project) => {
                let mut stmt = conn.prepare_cached(SELECT_WARNINGS_IN_PROJECT)?;
                collect(&mut stmt, [project.as_str()], warning_from_row)
            }
            None => {
                let mut stmt = conn.prepare_cached(SELECT_WARNINGS)?;
                collect(&mut stmt, [], warning_from_row)
            }
        }
    }

    /// Enforce `policy`. Safe and cheap to call on a timer; see
    /// [`crate::RetentionPolicy`] for what "bounded" means here.
    pub fn apply_retention(
        &self,
        policy: &RetentionPolicy,
        now: SystemTime,
    ) -> Result<RetentionReport, StorageError> {
        retention::run(&self.conn(), policy, now)
    }
}

/// `query_map` insists the row decoder return a `rusqlite::Error`, but decoding
/// a JSON column produces a `serde_json::Error`. Driving the cursor by hand
/// keeps the real error instead of flattening it into a SQLite one.
fn collect<T, P: Params>(
    stmt: &mut Statement<'_>,
    params: P,
    decode: fn(&Row<'_>) -> Result<T, StorageError>,
) -> Result<Vec<T>, StorageError> {
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(decode(row)?);
    }
    Ok(out)
}

fn configure(conn: &Connection, on_disk: bool) -> Result<(), StorageError> {
    // Two writers can exist during a daemon restart, and a lock error there
    // would look like data loss to the user.
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;

    if on_disk {
        // `journal_mode` answers with the mode it settled on, so it has to be
        // queried rather than set blind.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(
                journal_mode = %mode,
                "WAL unavailable; readers will block writers"
            );
        }
        // NORMAL: a crash may cost the last few commits but cannot corrupt the
        // file. Paying an fsync per second-by-second sample is not worth it for
        // observability data that is regenerated within one poll interval.
        conn.pragma_update(None, "synchronous", 1)?;
    }
    Ok(())
}

fn project_from_row(row: &Row<'_>) -> Result<Project, StorageError> {
    let evidence: String = row.get("evidence")?;
    Ok(Project {
        id: ProjectId::from_stored(row.get::<_, String>("id")?),
        root: PathBuf::from(row.get::<_, String>("root")?),
        name: row.get("name")?,
        kind: codec::decode(&row.get::<_, String>("kind")?)?,
        confidence: row.get::<_, f64>("confidence")? as f32,
        evidence: serde_json::from_str(&evidence)?,
        first_seen: from_millis(row.get("first_seen")?),
        last_seen: from_millis(row.get("last_seen")?),
    })
}

fn service_from_row(row: &Row<'_>) -> Result<Service, StorageError> {
    let instances: String = row.get("instances")?;
    let endpoints: String = row.get("endpoints")?;
    Ok(Service {
        id: ServiceId::from_stored(row.get::<_, String>("id")?),
        project_id: row
            .get::<_, Option<String>>("project_id")?
            .map(ProjectId::from_stored),
        name: row.get("name")?,
        kind: codec::decode::<ServiceKind>(&row.get::<_, String>("kind")?)?,
        runtime: codec::decode(&row.get::<_, String>("runtime")?)?,
        fingerprint: row.get("fingerprint")?,
        health: codec::decode::<Health>(&row.get::<_, String>("health")?)?,
        instances: serde_json::from_str(&instances)?,
        endpoints: serde_json::from_str(&endpoints)?,
        first_seen: from_millis(row.get("first_seen")?),
        last_seen: from_millis(row.get("last_seen")?),
        restart_count: row.get("restart_count")?,
    })
}

fn event_from_row(row: &Row<'_>) -> Result<DevPulseEvent, StorageError> {
    Ok(DevPulseEvent {
        id: EventId::from_stored(row.get::<_, String>("id")?),
        at: from_millis(row.get("at")?),
        project_id: row
            .get::<_, Option<String>>("project_id")?
            .map(ProjectId::from_stored),
        kind: codec::decode::<EventKind>(&row.get::<_, String>("kind")?)?,
    })
}

fn warning_from_row(row: &Row<'_>) -> Result<Warning, StorageError> {
    let related: String = row.get("related_events")?;
    Ok(Warning {
        id: row.get("id")?,
        rule: row.get("rule")?,
        severity: codec::decode::<Severity>(&row.get::<_, String>("severity")?)?,
        project_id: row
            .get::<_, Option<String>>("project_id")?
            .map(ProjectId::from_stored),
        service_id: row
            .get::<_, Option<String>>("service_id")?
            .map(ServiceId::from_stored),
        message: row.get("message")?,
        first_seen: from_millis(row.get("first_seen")?),
        last_seen: from_millis(row.get("last_seen")?),
        related_events: serde_json::from_str(&related)?,
    })
}

/// A read-back of persisted resource history. Kept separate from the writing
/// API because the daemon's *live* ring buffer lives in `devpulse-core`; this is
/// only for showing history the daemon did not itself observe, such as after a
/// restart.
impl Store {
    /// The newest `limit` samples for one service, returned oldest first
    /// because that is the order a chart consumes.
    ///
    /// `limit` is mandatory rather than optional: retention is a policy the
    /// caller may forget to run, and an unbounded read of a forgotten table is
    /// exactly the kind of surprise `AGENTS.md` rule 7 forbids.
    pub fn resource_samples(
        &self,
        service: &ServiceId,
        limit: usize,
    ) -> Result<Vec<ResourceSample>, StorageError> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT at, cpu_percent, memory_bytes FROM resource_samples \
             WHERE service_id = ?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let mut samples = collect(&mut stmt, (service.as_str(), count_to_sql(limit)), |row| {
            Ok(ResourceSample {
                at: from_millis(row.get("at")?),
                cpu_percent: row.get::<_, f64>("cpu_percent")? as f32,
                memory_bytes: bytes_from_sql(row.get("memory_bytes")?),
            })
        })?;
        samples.reverse();
        Ok(samples)
    }
}
