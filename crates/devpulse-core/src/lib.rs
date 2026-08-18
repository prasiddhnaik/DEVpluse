//! DevPulse domain layer.
//!
//! This crate is deliberately platform-free: it never touches process or socket
//! APIs. It owns the vocabulary the rest of the system agrees on.
//!
//! * [`ids`] — typed, mostly derived identifiers.
//! * [`model`] — `Project`, `Service`, `Connection`, events, warnings.
//! * [`identity`] — stable service fingerprints that survive a restart.
//! * [`project`] — resolve a working directory to a project root, with evidence.
//! * [`grouping`] — turn observations into project memberships.
//! * [`redact`] — strip likely secrets out of process command lines.
//! * [`service_filter`] — which processes count as services at all.

pub mod grouping;
pub mod identity;
pub mod ids;
pub mod model;
pub mod project;
pub mod redact;
pub mod registry;
pub mod resources;
pub mod service_filter;
pub mod topology;

pub use grouping::{
    GroupingEngine, GroupingInput, GroupingOutcome, Membership, MembershipEvidence,
};
pub use identity::{ContainerIdentity, Runtime, ServiceFingerprint};
pub use ids::{ConnectionId, EventId, ProjectId, ServiceId};
pub use model::{
    Connection, DevPulseEvent, Endpoint, EventKind, Evidence, EvidenceType, Health,
    ProcessInstance, Project, Protocol, ResourceSample, Service, ServiceKind, Severity, Warning,
};
pub use project::{
    MarkerHit, NoProject, ProjectEvidence, ProjectMarker, ProjectMatch, ProjectResolver,
    ResolverConfig, RootKind,
};
pub use redact::{REDACTED, redact_command};
pub use registry::{RegistryDelta, ServiceObservation, ServiceRegistry};
pub use resources::ResourceHistory;
pub use service_filter::{
    MIN_PORTLESS_LIFETIME, is_build_tool, is_bundled_app, is_service_process, is_system_tool,
};
pub use topology::{ObservedConnectionEndpoints, Topology, TopologyBuilder, TopologyDelta};
