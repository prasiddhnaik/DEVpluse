//! Runscape local persistence (milestone 5).
//!
//! One SQLite file holds what must survive a daemon restart: projects, stable
//! services, the recent event timeline, warnings, the aliases the developer
//! typed, and a bounded slice of resource history. Live runtime state is *not*
//! here — the registry and topology are rebuilt from observation on every start,
//! because a persisted PID is a lie the moment the process exits
//! (`AGENTS.md` rule 5).
//!
//! # Why SQLite, and why `rusqlite`
//!
//! The data is small, local, and queried by one process. A single file the
//! developer can open with `sqlite3` beats a server they have to install, and
//! `rusqlite` with the `bundled` feature means no system SQLite is required on
//! any of the three target platforms.
//!
//! # Shape of the API
//!
//! Writes are upserts keyed on the domain's own stable ids, so the daemon can
//! replay a snapshot without producing duplicates. Reads reconstruct whole
//! domain values; nothing returns half a `Service` and expects the caller to
//! fill in the rest.
//!
//! All methods take `&self` and block. See [`Store`] for the concurrency and
//! blocking contract.
//!
//! ```no_run
//! use std::time::SystemTime;
//! use runscape_storage::{RetentionPolicy, Store};
//!
//! # fn main() -> Result<(), runscape_storage::StorageError> {
//! let store = Store::open(std::path::Path::new("/tmp/runscape/state.db"))?;
//! let report = store.apply_retention(&RetentionPolicy::default(), SystemTime::now())?;
//! println!("pruned {} events", report.events_deleted);
//! # Ok(())
//! # }
//! ```

mod codec;
mod error;
mod retention;
mod schema;
mod store;

pub use error::StorageError;
pub use retention::{RetentionPolicy, RetentionReport};
pub use schema::{SCHEMA_DDL, SCHEMA_VERSION};
pub use store::Store;
