//! One error type for the whole crate.
//!
//! Callers of a local database can do almost nothing useful with the difference
//! between a busy timeout and a malformed page, so the SQLite error is kept
//! whole (`source` chains to it) rather than mapped into a taxonomy nobody
//! matches on. The two variants that *are* actionable get their own shapes: a
//! database written by a newer DevPulse must not be silently downgraded, and a
//! path that cannot hold a database must be reported before the daemon starts
//! recording into nowhere.

use std::path::PathBuf;

/// Anything that can go wrong while persisting or reading DevPulse state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The underlying SQLite call failed.
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    /// An enum-shaped or JSON column could not be encoded or decoded. In
    /// practice this means the value contains a non-UTF-8 path, or the file was
    /// hand-edited.
    #[error("could not encode or decode a JSON column")]
    Json(#[from] serde_json::Error),

    /// The database directory could not be created.
    #[error("could not create the database directory {path}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The file was written by a newer DevPulse. Opening it anyway would mean
    /// writing rows the newer schema does not expect, so we refuse.
    #[error(
        "database schema version {found} is newer than the supported version {supported}; \
         upgrade DevPulse or remove the database file"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    /// No migration exists for the recorded version. Only reachable if the
    /// `schema_version` row was corrupted, since every version this build ever
    /// wrote has a path forward.
    #[error("no migration from schema version {from}; the database may be corrupt")]
    Migration { from: u32 },
}
