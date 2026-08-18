//! Schema creation and the migration path.
//!
//! # How versioning works
//!
//! `schema_version` holds exactly one row. A fresh database gets the frozen
//! baseline in `schema.sql` plus that row; an existing database is only ever
//! moved forward by the numbered steps in [`MIGRATIONS`].
//!
//! Adding version *n + 1* means: append a function to [`MIGRATIONS`] that turns
//! version *n* into *n + 1*, bump [`SCHEMA_VERSION`], and leave `schema.sql`
//! alone. The baseline must stay frozen because `CREATE TABLE IF NOT EXISTS`
//! does nothing to a table that already exists, so editing it would upgrade new
//! installations and quietly skip every existing one.
//!
//! A database from a *newer* DevPulse is refused rather than used: writing v1
//! rows into a v2 schema is how data gets lost.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::codec::to_millis;
use crate::error::StorageError;

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

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

/// Empty while version 1 is current. See the module docs before adding to it.
const MIGRATIONS: &[Migration] = &[];

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
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA_DDL)?;
            tx.execute(
                "INSERT INTO schema_version (id, version, applied_at) VALUES (1, ?1, ?2)",
                params![SCHEMA_VERSION, to_millis(now)],
            )?;
            tx.commit()?;
            Ok(SCHEMA_VERSION)
        }
        Some(found) if found == SCHEMA_VERSION => Ok(found),
        Some(found) if found > SCHEMA_VERSION => Err(StorageError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        }),
        Some(found) => {
            let tx = conn.transaction()?;
            for from in found..SCHEMA_VERSION {
                let step = usize::try_from(from)
                    .ok()
                    .and_then(|from| from.checked_sub(1))
                    .and_then(|index| MIGRATIONS.get(index))
                    .ok_or(StorageError::Migration { from })?;
                step(&tx)?;
            }
            tx.execute(
                "UPDATE schema_version SET version = ?1, applied_at = ?2 WHERE id = 1",
                params![SCHEMA_VERSION, to_millis(now)],
            )?;
            tx.commit()?;
            Ok(SCHEMA_VERSION)
        }
    }
}
