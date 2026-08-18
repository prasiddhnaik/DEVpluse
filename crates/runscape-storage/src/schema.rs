//! Schema creation and the migration path.
//!
//! # How versioning works
//!
//! `schema_version` holds exactly one row. A fresh database gets the frozen
//! baseline in `schema.sql` (version 1), then walks [`MIGRATIONS`] up to
//! [`SCHEMA_VERSION`]. An existing database is only ever moved forward by
//! those same numbered steps.
//!
//! Adding version *n + 1* means: append a function to [`MIGRATIONS`] that turns
//! version *n* into *n + 1*, bump [`SCHEMA_VERSION`], and leave `schema.sql`
//! alone. The baseline must stay frozen because `CREATE TABLE IF NOT EXISTS`
//! does nothing to a table that already exists, so editing it would upgrade new
//! installations and quietly skip every existing one.
//!
//! A database from a *newer* Runscape is refused rather than used: writing v1
//! rows into a v2 schema is how data gets lost.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::codec::to_millis;
use crate::error::StorageError;

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 3;

/// The frozen baseline DDL, applied to a brand new database.
pub const SCHEMA_DDL: &str = include_str!("../schema.sql");

/// Bootstrapped separately from the baseline so the version can be read before
/// deciding whether the baseline should be applied at all.
const VERSION_DDL: &str = "\
CREATE TABLE IF NOT EXISTS schema_version (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    version    INTEGER NOT NULL,
    applied_at INTEGER NOT NULL
) STRICT;";

/// Migrates a database from version *index + 1* to version *index + 2*.
type Migration = fn(&Transaction<'_>) -> Result<(), StorageError>;

/// Extra resource columns. Additive with defaults so a v1 file keeps loading.
fn migrate_v1_to_v2(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch(
        "ALTER TABLE resource_samples ADD COLUMN virtual_memory_bytes INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE resource_samples ADD COLUMN thread_count INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE resource_samples ADD COLUMN disk_read_bytes INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE resource_samples ADD COLUMN disk_write_bytes INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

/// Observed connection count per sample. Additive with a zero default.
fn migrate_v2_to_v3(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute_batch(
        "ALTER TABLE resource_samples ADD COLUMN connection_count INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

const MIGRATIONS: &[Migration] = &[migrate_v1_to_v2, migrate_v2_to_v3];

/// Bring `conn` up to [`SCHEMA_VERSION`], doing nothing if it is already there.
///
/// Safe to call on every open: that is the only way a long-lived local database
/// stays correct across upgrades.
pub(crate) fn apply(conn: &mut Connection, now: SystemTime) -> Result<u32, StorageError> {
    conn.execute_batch(VERSION_DDL)?;

    let found: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_version WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    // A garbage value (negative, or wider than u32) is treated as "from the
    // future": refusing is the only safe reading of a version we cannot parse.
    let found = found.map(|raw| u32::try_from(raw).unwrap_or(u32::MAX));

    match found {
        None => {
            // Baseline is frozen at v1. A brand-new file is stamped as v1 and
            // then walked forward so it does not skip migrations.
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA_DDL)?;
            tx.execute(
                "INSERT INTO schema_version (id, version, applied_at) VALUES (1, 1, ?1)",
                params![to_millis(now)],
            )?;
            tx.commit()?;
            migrate(conn, 1, now)
        }
        Some(found) if found == SCHEMA_VERSION => Ok(found),
        Some(found) if found > SCHEMA_VERSION => Err(StorageError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        }),
        Some(found) => migrate(conn, found, now),
    }
}

fn migrate(conn: &mut Connection, from: u32, now: SystemTime) -> Result<u32, StorageError> {
    let tx = conn.transaction()?;
    for version in from..SCHEMA_VERSION {
        let step = usize::try_from(version)
            .ok()
            .and_then(|version| version.checked_sub(1))
            .and_then(|index| MIGRATIONS.get(index))
            .ok_or(StorageError::Migration { from: version })?;
        step(&tx)?;
    }
    tx.execute(
        "UPDATE schema_version SET version = ?1, applied_at = ?2 WHERE id = 1",
        params![SCHEMA_VERSION, to_millis(now)],
    )?;
    tx.commit()?;
    Ok(SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn a_fresh_database_lands_on_the_current_schema() {
        let mut conn = Connection::open_in_memory().expect("open");
        let version = apply(&mut conn, SystemTime::UNIX_EPOCH).expect("apply");
        assert_eq!(version, SCHEMA_VERSION);
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(resource_samples)")
            .expect("pragma")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("map")
            .collect::<Result<_, _>>()
            .expect("columns");
        for name in [
            "virtual_memory_bytes",
            "thread_count",
            "disk_read_bytes",
            "disk_write_bytes",
            "connection_count",
        ] {
            assert!(columns.iter().any(|c| c == name), "missing {name}");
        }
    }

    #[test]
    fn a_v1_file_keeps_old_samples_and_gains_zero_extra_columns() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(VERSION_DDL).expect("version table");
        conn.execute_batch(SCHEMA_DDL).expect("v1 ddl");
        conn.execute(
            "INSERT INTO schema_version (id, version, applied_at) VALUES (1, 1, 0)",
            [],
        )
        .expect("stamp v1");
        conn.execute(
            "INSERT INTO resource_samples (service_id, at, cpu_percent, memory_bytes)
             VALUES ('svc_old', 1000, 4.5, 2048)",
            [],
        )
        .expect("legacy row");

        let version = apply(&mut conn, SystemTime::UNIX_EPOCH).expect("migrate");
        assert_eq!(version, SCHEMA_VERSION);

        let (virt, threads, read, write, connections): (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT virtual_memory_bytes, thread_count, disk_read_bytes, disk_write_bytes,
                        connection_count
                 FROM resource_samples WHERE service_id = 'svc_old'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("legacy values");
        assert_eq!((virt, threads, read, write, connections), (0, 0, 0, 0, 0));
    }

    #[test]
    fn a_v2_file_gains_connection_count_at_zero() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(VERSION_DDL).expect("version table");
        conn.execute_batch(SCHEMA_DDL).expect("v1 ddl");
        conn.execute(
            "INSERT INTO schema_version (id, version, applied_at) VALUES (1, 2, 0)",
            [],
        )
        .expect("stamp v2");
        conn.execute_batch(
            "ALTER TABLE resource_samples ADD COLUMN virtual_memory_bytes INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE resource_samples ADD COLUMN thread_count INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE resource_samples ADD COLUMN disk_read_bytes INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE resource_samples ADD COLUMN disk_write_bytes INTEGER NOT NULL DEFAULT 0;",
        )
        .expect("v2 columns");
        conn.execute(
            "INSERT INTO resource_samples (service_id, at, cpu_percent, memory_bytes)
             VALUES ('svc_v2', 2000, 1.0, 512)",
            [],
        )
        .expect("v2 row");

        let version = apply(&mut conn, SystemTime::UNIX_EPOCH).expect("migrate");
        assert_eq!(version, SCHEMA_VERSION);
        let count: i64 = conn
            .query_row(
                "SELECT connection_count FROM resource_samples WHERE service_id = 'svc_v2'",
                [],
                |row| row.get(0),
            )
            .expect("connection_count");
        assert_eq!(count, 0);
    }
}
