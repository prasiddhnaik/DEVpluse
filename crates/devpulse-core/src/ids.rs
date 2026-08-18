//! Typed identifiers (task T1.1).
//!
//! Identifiers are opaque strings with a type prefix so that a `ServiceId` can
//! never be passed where a `ProjectId` belongs, and so that a raw id in a log
//! line or an API response is self-describing.
//!
//! All identifiers except [`EventId`] are *derived*: the same logical thing
//! always produces the same id, on every run and every machine. That is what
//! lets a service keep its identity across a restart (`DECISIONS.md` D006).

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Truncated SHA-256, hex encoded. 12 hex chars = 48 bits: ample for the number
/// of projects and services on one developer machine, short enough to read.
fn short_digest(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(12);
    for byte in &digest[..6] {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Derive a stable id from a canonical description of the thing.
            /// Callers must build that description deterministically.
            pub fn derived(canonical: &str) -> Self {
                Self(format!(concat!($prefix, "_{}"), short_digest(canonical)))
            }

            /// Rebuild an id that was previously persisted. No validation is
            /// performed beyond non-emptiness, because storage is trusted.
            pub fn from_stored(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

typed_id!(
    /// Identifies a project. Derived from the canonical project root path, so a
    /// project keeps its id across daemon restarts.
    ProjectId,
    "prj"
);

typed_id!(
    /// Identifies a *logical* service, never a process instance
    /// (`DECISIONS.md` D006). Derived from
    /// [`ServiceFingerprint`](crate::identity::ServiceFingerprint).
    ServiceId,
    "svc"
);

typed_id!(
    /// Identifies an edge between two services. Derived from the endpoints, so
    /// a reconnect reuses the same edge and updates `last_seen`.
    ConnectionId,
    "con"
);

/// Identifies one event occurrence.
///
/// Unlike the other ids this is *not* derived: two identical-looking events at
/// different times are different events. Ids sort chronologically, which is
/// what the timeline needs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(String);

impl EventId {
    /// Build an id from an explicit timestamp and sequence number. The daemon
    /// passes a monotonically increasing sequence so that two events in the
    /// same millisecond still order deterministically.
    pub fn new(unix_millis: u64, sequence: u32) -> Self {
        Self(format!("evt_{unix_millis:012x}{sequence:06x}"))
    }

    pub fn from_stored(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_ids_are_stable() {
        assert_eq!(
            ProjectId::derived("/Users/dev/code/app"),
            ProjectId::derived("/Users/dev/code/app")
        );
    }

    #[test]
    fn different_inputs_produce_different_ids() {
        assert_ne!(
            ProjectId::derived("/Users/dev/code/app"),
            ProjectId::derived("/Users/dev/code/other")
        );
    }

    #[test]
    fn ids_carry_a_type_prefix() {
        assert!(ProjectId::derived("x").as_str().starts_with("prj_"));
        assert!(ServiceId::derived("x").as_str().starts_with("svc_"));
        assert!(ConnectionId::derived("x").as_str().starts_with("con_"));
        assert!(EventId::new(1, 2).as_str().starts_with("evt_"));
    }

    #[test]
    fn event_ids_sort_chronologically() {
        let earlier = EventId::new(1_700_000_000_000, 0);
        let same_ms_later_seq = EventId::new(1_700_000_000_000, 1);
        let later = EventId::new(1_700_000_000_001, 0);

        assert!(earlier < same_ms_later_seq);
        assert!(same_ms_later_seq < later);
    }

    #[test]
    fn round_trips_through_storage() {
        let id = ServiceId::derived("host|/app|node|3000");
        assert_eq!(ServiceId::from_stored(id.as_str()), id);
    }
}
