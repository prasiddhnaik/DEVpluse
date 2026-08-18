//! DevPulse Docker inspection (Milestone 6).
//!
//! Docker is optional. Everything here is built around that: detection returns a
//! state rather than an error ([`DockerAvailability`]), and the daemon that
//! cannot inspect Docker simply reports fewer services instead of failing.
//!
//! ```text
//! DockerAvailability::detect()
//!   ├── Unavailable { reason }   → report the reason, carry on
//!   └── Available(collector)     → collector.snapshot() → ContainerSnapshot
//!                                    └── ObservedContainer::to_service(…) → Service
//! ```
//!
//! Containers reach the graph through the same [`Service`](devpulse_core::Service)
//! shape as host processes (`TASKS.md` T6.3), and their identity comes from the
//! Compose labels so it survives `docker compose up` recreating the container.
//!
//! CPU and memory are opt-in — see
//! [`BollardCollector::with_stats`] for why, and what it costs.

pub mod availability;
pub mod collector;
pub mod container;
pub mod error;

pub use availability::DockerAvailability;
pub use collector::{BollardCollector, ContainerCollector};
pub use container::{
    COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, ContainerPort, ContainerSnapshot, ContainerState,
    ObservedContainer, container_identity,
};
pub use devpulse_core::ContainerIdentity;
pub use error::DockerError;
