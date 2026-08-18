//! WebSocket frames (`docs/api-contract.md`, task T3.4).
//!
//! The socket is the live channel: one `snapshot` on connect, incremental
//! frames afterwards. Frames are tagged JSON so a client can match on `type`
//! and ignore anything it does not know yet.

use serde::{Deserialize, Serialize};

use crate::dto::{ConnectionDto, EventDto, ProjectSummaryDto, ServiceDto, StatusDto, WarningDto};

/// Server → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Sent exactly once on connect, and again on request. Everything the
    /// dashboard needs for a cold start.
    Snapshot {
        at: String,
        status: Box<StatusDto>,
        projects: Vec<ProjectSummaryDto>,
        services: Vec<ServiceDto>,
        connections: Vec<ConnectionDto>,
        warnings: Vec<WarningDto>,
    },
    Events {
        at: String,
        events: Vec<EventDto>,
    },
    /// Full objects for the services that changed — not patches. A dashboard
    /// that misses a frame is wrong for one tick, not permanently.
    ServicesChanged {
        at: String,
        services: Vec<ServiceDto>,
        removed: Vec<String>,
    },
    TopologyChanged {
        at: String,
        project_id: Option<String>,
        added: Vec<ConnectionDto>,
        removed: Vec<String>,
    },
    WarningsChanged {
        at: String,
        warnings: Vec<WarningDto>,
        removed: Vec<String>,
    },
}

/// Client → server. The only thing a client may ask for is a fresh snapshot
/// (`DECISIONS.md` D004: the API exposes no control surface). Anything else is
/// ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Resnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frames_parse_by_tag() {
        let frame: ClientFrame =
            serde_json::from_str(r#"{"type":"resnapshot"}"#).expect("resnapshot parses");
        assert!(matches!(frame, ClientFrame::Resnapshot));
    }

    #[test]
    fn unknown_client_frames_are_rejected_not_guessed() {
        assert!(serde_json::from_str::<ClientFrame>(r#"{"type":"kill","pid":1}"#).is_err());
        assert!(serde_json::from_str::<ClientFrame>("not json").is_err());
    }

    #[test]
    fn server_frames_are_tagged() {
        let frame = ServerFrame::Events {
            at: "2026-08-17T10:00:00Z".to_string(),
            events: Vec::new(),
        };
        let json = serde_json::to_value(&frame).expect("serialises");
        assert_eq!(json["type"], "events");
    }
}
