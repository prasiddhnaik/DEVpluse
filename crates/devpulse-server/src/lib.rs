//! DevPulse daemon runtime and API.

pub mod api;
pub mod daemon;
pub mod dto;
pub mod frames;
pub mod persistence;
pub mod security;
pub mod snapshot;
pub mod state;
pub mod ws;

pub use daemon::{Daemon, DaemonConfig};
pub use devpulse_core::Service;
pub use security::{OriginPolicy, default_bind_addr};
pub use snapshot::{
    DEFAULT_TICK_INTERVAL, SnapshotConfig, SnapshotError, SnapshotLoop, TickResult,
};
pub use state::AppState;
