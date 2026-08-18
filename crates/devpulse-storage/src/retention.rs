//! Bounded retention (task T5.2).
//!
//! DevPulse watches a developer machine at roughly one sample per second per
//! service. Kept forever that is hundreds of millions of rows a year for a tool
//! nobody asked to be a time-series database, so both the timeline and the
//! resource history are bounded twice: by age, because a week-old CPU reading
//! answers no question anyone asks, and by row count, because a hundred noisy
//! services must not be able to fill the disk between two age cutoffs.
//!
//! Deletion runs in bounded batches, each its own implicit transaction. A
//! retention pass is idempotent, so an interrupted pass simply finishes on the
//! next tick — worth far more than the atomicity of holding a write lock over
//! fifty thousand deletes while the daemon is trying to record events.

use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::codec::{count_to_sql, to_millis};
use crate::error::StorageError;

/// Rows removed per statement. Small enough that the write lock is never held
/// for long, large enough that a full pass is a handful of statements.
const BATCH: usize = 512;

/// How much history to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Hard cap on stored events, applied after the age cutoff.
    pub max_events: usize,
    /// Events older than this are dropped.
    pub event_max_age: Duration,
    /// Resource samples older than this are dropped.
    pub resource_sample_max_age: Duration,
    /// Hard cap on stored samples per service, applied after the age cutoff.
    pub max_resource_samples_per_service: usize,
}

impl Default for RetentionPolicy {
    /// One development day of events and one hour of resource history.
    ///
    /// A day covers "what changed since I started working"; the row caps are
    /// the same windows expressed as an upper bound on volume — 50k events, and
    /// one hour at one sample per second per service.
    fn default() -> Self {
        Self {
            max_events: 50_000,
            event_max_age: Duration::from_secs(24 * 60 * 60),
            resource_sample_max_age: Duration::from_secs(60 * 60),
            max_resource_samples_per_service: 3_600,
        }
    }
}

/// What a retention pass actually removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub events_deleted: usize,
    pub samples_deleted: usize,
}

/// Apply `policy` as of `now`.
pub(crate) fn run(
    conn: &Connection,
    policy: &RetentionPolicy,
    now: SystemTime,
) -> Result<RetentionReport, StorageError> {
    let mut report = RetentionReport::default();

    report.events_deleted += delete_older_than(
        conn,
        "DELETE FROM events WHERE rowid IN \
         (SELECT rowid FROM events WHERE at < ?1 ORDER BY at ASC LIMIT ?2)",
        cutoff(now, policy.event_max_age),
    )?;
    report.events_deleted += trim_to(
        conn,
        "SELECT COUNT(*) FROM events",
        "DELETE FROM events WHERE rowid IN \
         (SELECT rowid FROM events ORDER BY at ASC, id ASC LIMIT ?1)",
        policy.max_events,
    )?;

    report.samples_deleted += delete_older_than(
        conn,
        "DELETE FROM resource_samples WHERE rowid IN \
         (SELECT rowid FROM resource_samples WHERE at < ?1 ORDER BY at ASC LIMIT ?2)",
        cutoff(now, policy.resource_sample_max_age),
    )?;
    report.samples_deleted += trim_samples_per_service(conn, policy)?;

    Ok(report)
}

/// Timestamp arithmetic in the stored domain: `SystemTime` subtraction can
/// underflow near the epoch, an `i64` millisecond subtraction cannot.
fn cutoff(now: SystemTime, age: Duration) -> i64 {
    let age = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
    to_millis(now).saturating_sub(age)
}

fn delete_older_than(
    conn: &Connection,
    sql: &str,
    cutoff_millis: i64,
) -> Result<usize, StorageError> {
    let mut stmt = conn.prepare(sql)?;
    let mut deleted = 0;
    loop {
        let batch = stmt.execute((cutoff_millis, count_to_sql(BATCH)))?;
        deleted += batch;
        // A short batch means the inner LIMIT ran out of matching rows.
        if batch < BATCH {
            return Ok(deleted);
        }
    }
}

/// Delete oldest-first until at most `keep` rows remain.
fn trim_to(
    conn: &Connection,
    count_sql: &str,
    delete_sql: &str,
    keep: usize,
) -> Result<usize, StorageError> {
    let total: i64 = conn.query_row(count_sql, [], |row| row.get(0))?;
    let total = usize::try_from(total).unwrap_or(0);
    let mut excess = total.saturating_sub(keep);
    if excess == 0 {
        return Ok(0);
    }

    let mut stmt = conn.prepare(delete_sql)?;
    let mut deleted = 0;
    while excess > 0 {
        let batch = stmt.execute([count_to_sql(excess.min(BATCH))])?;
        // Nothing left to delete: another writer got there first.
        if batch == 0 {
            break;
        }
        deleted += batch;
        excess -= batch.min(excess);
    }
    Ok(deleted)
}

/// The per-service cap has to be applied per service, not globally: one chatty
/// service must not evict another's history.
fn trim_samples_per_service(
    conn: &Connection,
    policy: &RetentionPolicy,
) -> Result<usize, StorageError> {
    let keep = policy.max_resource_samples_per_service;

    let mut over_cap = conn.prepare(
        "SELECT service_id, COUNT(*) FROM resource_samples \
         GROUP BY service_id HAVING COUNT(*) > ?1",
    )?;
    let mut rows = over_cap.query([count_to_sql(keep)])?;
    let mut excesses: Vec<(String, usize)> = Vec::new();
    while let Some(row) = rows.next()? {
        let service: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        let count = usize::try_from(count).unwrap_or(0);
        excesses.push((service, count.saturating_sub(keep)));
    }
    drop(rows);
    drop(over_cap);

    let mut stmt = conn.prepare(
        "DELETE FROM resource_samples WHERE rowid IN \
         (SELECT rowid FROM resource_samples WHERE service_id = ?1 ORDER BY at ASC LIMIT ?2)",
    )?;
    let mut deleted = 0;
    for (service, mut excess) in excesses {
        while excess > 0 {
            let batch = stmt.execute((&service, count_to_sql(excess.min(BATCH))))?;
            if batch == 0 {
                break;
            }
            deleted += batch;
            excess -= batch.min(excess);
        }
    }
    Ok(deleted)
}
