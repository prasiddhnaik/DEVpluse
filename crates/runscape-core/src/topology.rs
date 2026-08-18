//! Topology construction (task T2.3).
//!
//! Turns observed sockets into service-to-service edges.
//!
//! # The one rule
//!
//! An edge is emitted only when **both** ends are known services. If the client
//! PID belongs to no known service, or the port connected to is not a port any
//! known service listens on, nothing is emitted. A half-known connection is not
//! rendered as a mystery node, and it is never inferred into a plausible
//! neighbour (`AGENTS.md` rule 3, `DECISIONS.md` D007).
//!
//! # Direction
//!
//! Edges are derived from the **client side** only: the socket whose
//! `remote_port` matches a known listening port. The server side of the same
//! connection describes the same edge and would only produce a duplicate.
//!
//! A consequence worth stating: if the client is a process Runscape cannot see
//! (another user, or root), the connection is invisible even though the server
//! side is observable. Reporting "something connected to your database" without
//! being able to say what would be noise.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::ids::{ConnectionId, ServiceId};
use crate::model::{Connection, Evidence, EvidenceType, Service};

/// One established socket, as observed by the socket collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedConnectionEndpoints {
    pub local_addr: IpAddr,
    pub local_port: u16,
    pub remote_addr: IpAddr,
    pub remote_port: u16,
    /// Owning PID, when the OS disclosed it. `None` means the edge cannot be
    /// attributed and will be dropped.
    pub pid: Option<u32>,
}

/// The current set of edges.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    connections: Vec<Connection>,
}

impl Topology {
    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    pub fn len(&self) -> usize {
        self.connections.len()
    }

    pub fn get(&self, id: &ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| &c.id == id)
    }

    /// Edges touching a service, in either direction.
    pub fn touching(&self, service: &ServiceId) -> impl Iterator<Item = &Connection> {
        self.connections
            .iter()
            .filter(move |c| &c.source == service || &c.target == service)
    }
}

/// What changed between two topologies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopologyDelta {
    pub added: Vec<Connection>,
    pub removed: Vec<ConnectionId>,
}

impl TopologyDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Builds topologies and keeps evidence windows continuous across rebuilds.
#[derive(Debug, Default)]
pub struct TopologyBuilder {
    current: Topology,
}

impl TopologyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn topology(&self) -> &Topology {
        &self.current
    }

    /// Rebuild from the latest observation and report the change.
    ///
    /// An edge that survives keeps its original `first_seen` and advances its
    /// `last_seen`: the developer needs to know a dependency has existed since
    /// the service started, not since the last poll.
    pub fn observe(
        &mut self,
        services: &[&Service],
        established: &[ObservedConnectionEndpoints],
        at: SystemTime,
    ) -> TopologyDelta {
        let index = ServiceIndex::build(services);
        let mut next: BTreeMap<ConnectionId, Connection> = BTreeMap::new();

        for socket in established {
            let Some(edge) = index.edge_for(socket) else {
                continue;
            };
            let (source, target, port) = edge;
            let id = Connection::id_for(&source, &target, port);

            // An edge seen twice in one batch (several sockets between the same
            // pair) is one edge, not two.
            if next.contains_key(&id) {
                continue;
            }

            let evidence = match self.current.get(&id) {
                Some(previous) => {
                    let mut evidence = previous.evidence.clone();
                    evidence.observe_again(at);
                    evidence
                }
                None => Evidence::observed(EvidenceType::ObservedSocket, at),
            };
            next.insert(id, Connection::new(source, target, port, evidence));
        }

        let mut delta = TopologyDelta::default();
        for connection in next.values() {
            if self.current.get(&connection.id).is_none() {
                delta.added.push(connection.clone());
            }
        }
        for previous in &self.current.connections {
            if !next.contains_key(&previous.id) {
                delta.removed.push(previous.id.clone());
            }
        }

        self.current = Topology {
            connections: next.into_values().collect(),
        };
        delta
    }
}

/// Lookup tables built once per batch.
struct ServiceIndex {
    /// pid -> owning service.
    by_pid: BTreeMap<u32, ServiceId>,
    /// port -> the services listening on it, with their bind addresses.
    by_port: BTreeMap<u16, Vec<(IpAddr, ServiceId)>>,
}

impl ServiceIndex {
    fn build(services: &[&Service]) -> Self {
        let mut by_pid = BTreeMap::new();
        let mut by_port: BTreeMap<u16, Vec<(IpAddr, ServiceId)>> = BTreeMap::new();

        for service in services {
            for instance in &service.instances {
                by_pid.insert(instance.pid, service.id.clone());
            }
            for endpoint in &service.endpoints {
                by_port
                    .entry(endpoint.port)
                    .or_default()
                    .push((endpoint.address, service.id.clone()));
            }
        }

        Self { by_pid, by_port }
    }

    /// `(source, target, target_port)` for a client-side socket, or `None`.
    fn edge_for(
        &self,
        socket: &ObservedConnectionEndpoints,
    ) -> Option<(ServiceId, ServiceId, u16)> {
        let source = self.by_pid.get(&socket.pid?)?.clone();
        let target = self.listener_on(socket.remote_port, socket.remote_addr)?;

        // A service talking to itself is not an edge worth drawing, and it is
        // usually the server side of its own accepted connection.
        if source == target {
            return None;
        }
        Some((source, target, socket.remote_port))
    }

    /// Which service listens on `port` at `addr`.
    ///
    /// A listener bound to `0.0.0.0` or `::` accepts connections addressed to
    /// any local address, so it matches. When several distinct services listen
    /// on the same port and none matches exactly, the answer is ambiguous and
    /// we return `None` rather than picking one.
    fn listener_on(&self, port: u16, addr: IpAddr) -> Option<ServiceId> {
        let candidates = self.by_port.get(&port)?;

        if let Some((_, id)) = candidates.iter().find(|(bound, _)| *bound == addr) {
            return Some(id.clone());
        }
        let wildcard: Vec<&(IpAddr, ServiceId)> = candidates
            .iter()
            .filter(|(bound, _)| bound.is_unspecified())
            .collect();
        if let [(_, id)] = wildcard.as_slice() {
            return Some(id.clone());
        }

        let distinct: Vec<&ServiceId> = {
            let mut ids: Vec<&ServiceId> = candidates.iter().map(|(_, id)| id).collect();
            ids.sort();
            ids.dedup();
            ids
        };
        match distinct.as_slice() {
            [only] => Some((*only).clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Runtime;
    use crate::model::{Endpoint, Health, ProcessInstance, Protocol, ServiceKind};
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
    }

    fn service(name: &str, pid: u32, ports: &[u16], addr: IpAddr) -> Service {
        Service {
            id: ServiceId::derived(name),
            project_id: None,
            name: name.to_string(),
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Node,
            fingerprint: format!("host|{name}"),
            health: Health::Healthy,
            instances: vec![ProcessInstance {
                pid,
                parent_pid: None,
                executable: None,
                command: vec![],
                cwd: None,
                started_at_epoch_secs: 1,
                cpu_percent: 0.0,
                memory_bytes: 1,
                virtual_memory_bytes: 2,
                thread_count: None,
                disk_read_bytes: 0,
                disk_write_bytes: 0,
            }],
            endpoints: ports
                .iter()
                .map(|port| Endpoint {
                    address: addr,
                    port: *port,
                    protocol: Protocol::Tcp,
                    pid: Some(pid),
                })
                .collect(),
            first_seen: at(0),
            last_seen: at(0),
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        }
    }

    fn client_socket(pid: u32, ephemeral: u16, target_port: u16) -> ObservedConnectionEndpoints {
        ObservedConnectionEndpoints {
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            local_port: ephemeral,
            remote_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            remote_port: target_port,
            pid: Some(pid),
        }
    }

    fn server_socket(pid: u32, listen_port: u16, ephemeral: u16) -> ObservedConnectionEndpoints {
        ObservedConnectionEndpoints {
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            local_port: listen_port,
            remote_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            remote_port: ephemeral,
            pid: Some(pid),
        }
    }

    #[test]
    fn builds_an_edge_from_the_client_side() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(
            &[&web, &db],
            &[
                client_socket(100, 51000, 5432),
                server_socket(200, 5432, 51000),
            ],
            at(1),
        );

        assert_eq!(delta.added.len(), 1, "both sides describe one edge");
        let edge = &delta.added[0];
        assert_eq!(edge.source, web.id);
        assert_eq!(edge.target, db.id);
        assert_eq!(edge.target_port, 5432);
        assert_eq!(edge.evidence.evidence_type, EvidenceType::ObservedSocket);
        assert_eq!(edge.evidence.confidence, 1.0);
    }

    #[test]
    fn no_edge_when_the_client_is_unknown() {
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(
            &[&db],
            &[
                client_socket(999, 51000, 5432),
                server_socket(200, 5432, 51000),
            ],
            at(1),
        );

        assert!(
            delta.added.is_empty(),
            "an unattributable peer must not become an edge"
        );
    }

    #[test]
    fn no_edge_when_the_target_port_belongs_to_nobody() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(&[&web], &[client_socket(100, 51000, 9999)], at(1));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn no_edge_without_an_owning_pid() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let mut socket = client_socket(100, 51000, 5432);
        socket.pid = None;

        let delta = builder.observe(&[&web, &db], &[socket], at(1));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn a_service_talking_to_itself_is_not_an_edge() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(&[&web], &[client_socket(100, 51000, 3000)], at(1));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn several_sockets_between_the_same_pair_are_one_edge() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(
            &[&web, &db],
            &[
                client_socket(100, 51000, 5432),
                client_socket(100, 51001, 5432),
                client_socket(100, 51002, 5432),
            ],
            at(1),
        );
        assert_eq!(delta.added.len(), 1);
        assert_eq!(builder.topology().len(), 1);
    }

    #[test]
    fn a_surviving_edge_keeps_its_original_first_seen() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        builder.observe(&[&web, &db], &[client_socket(100, 51000, 5432)], at(1));
        // Reconnect on a different ephemeral port: same logical dependency.
        let delta = builder.observe(&[&web, &db], &[client_socket(100, 51999, 5432)], at(60));

        assert!(delta.is_empty(), "the same dependency must not re-add");
        let edge = &builder.topology().connections()[0];
        assert_eq!(edge.evidence.first_seen, at(1));
        assert_eq!(edge.evidence.last_seen, at(60));
    }

    #[test]
    fn a_disappearing_edge_is_removed() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let added = builder.observe(&[&web, &db], &[client_socket(100, 51000, 5432)], at(1));
        let removed = builder.observe(&[&web, &db], &[], at(2));

        assert_eq!(removed.removed, vec![added.added[0].id.clone()]);
        assert!(builder.topology().is_empty());
    }

    #[test]
    fn a_wildcard_listener_matches_a_loopback_connection() {
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(&[&web, &db], &[client_socket(100, 51000, 5432)], at(1));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].target, db.id);
    }

    #[test]
    fn an_ambiguous_port_produces_no_edge() {
        // Two services listening on the same port on different addresses, and a
        // connection to a third address: guessing would be worse than silence.
        let a = service("a", 200, &[5432], IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
        let b = service("b", 300, &[5432], IpAddr::V6(Ipv6Addr::LOCALHOST));
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = TopologyBuilder::new();

        let delta = builder.observe(&[&web, &a, &b], &[client_socket(100, 51000, 5432)], at(1));
        assert!(delta.added.is_empty());
    }

    #[test]
    fn topology_is_deterministic() {
        let web = service("web", 100, &[3000], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let db = service("db", 200, &[5432], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cache = service("cache", 300, &[6379], IpAddr::V4(Ipv4Addr::LOCALHOST));
        let sockets = vec![
            client_socket(100, 51000, 5432),
            client_socket(100, 51001, 6379),
        ];

        let mut first = TopologyBuilder::new();
        let mut second = TopologyBuilder::new();
        first.observe(&[&web, &db, &cache], &sockets, at(1));
        second.observe(&[&cache, &db, &web], &sockets, at(1));

        assert_eq!(first.topology(), second.topology());
    }
}
