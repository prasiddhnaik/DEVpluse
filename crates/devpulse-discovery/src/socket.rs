//! Socket and port discovery (task T0.3).
//!
//! The hard question this crate answers is *which process owns this port*.
//! `netstat2` provides that mapping through platform-specific back ends
//! (`libproc` on macOS, `/proc` on Linux) without shelling out to `lsof` or
//! `netstat`, and without touching packet payloads.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, get_sockets_info};
use serde::Serialize;

use crate::error::CollectorError;

/// Transport protocol of an observed socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

/// Normalised TCP state. UDP sockets are [`SocketState::Stateless`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketState {
    Listen,
    Established,
    SynSent,
    SynReceived,
    FinWait,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    Closed,
    /// UDP: no connection state exists.
    Stateless,
    /// The platform reported a state DevPulse does not model.
    Unknown,
}

impl From<TcpState> for SocketState {
    fn from(state: TcpState) -> Self {
        match state {
            TcpState::Listen => Self::Listen,
            TcpState::Established => Self::Established,
            TcpState::SynSent => Self::SynSent,
            TcpState::SynReceived => Self::SynReceived,
            TcpState::FinWait1 | TcpState::FinWait2 => Self::FinWait,
            TcpState::CloseWait => Self::CloseWait,
            TcpState::Closing => Self::Closing,
            TcpState::LastAck => Self::LastAck,
            TcpState::TimeWait => Self::TimeWait,
            TcpState::Closed | TcpState::DeleteTcb => Self::Closed,
            TcpState::Unknown => Self::Unknown,
        }
    }
}

/// One socket as observed at a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedSocket {
    pub protocol: Protocol,
    pub local_addr: IpAddr,
    pub local_port: u16,
    /// `None` for listening and stateless sockets.
    pub remote_addr: Option<IpAddr>,
    pub remote_port: Option<u16>,
    pub state: SocketState,
    /// Owning PIDs. Empty when the OS would not disclose the owner — that is a
    /// visibility limit, never an invitation to guess.
    pub pids: Vec<u32>,
}

impl ObservedSocket {
    pub fn is_listening(&self) -> bool {
        self.state == SocketState::Listen
    }

    pub fn is_established(&self) -> bool {
        self.state == SocketState::Established
    }

    /// True when both endpoints are on this machine's loopback interface.
    pub fn is_loopback_pair(&self) -> bool {
        self.local_addr.is_loopback() && self.remote_addr.is_some_and(|a| a.is_loopback())
    }

    pub fn owned_by(&self, pid: u32) -> bool {
        self.pids.contains(&pid)
    }
}

/// A full socket-table observation.
#[derive(Debug, Clone, Serialize)]
pub struct SocketSnapshot {
    pub captured_at: SystemTime,
    pub duration: Duration,
    pub sockets: Vec<ObservedSocket>,
    /// Sockets whose owning PID the OS would not disclose.
    pub sockets_without_owner: usize,
}

impl SocketSnapshot {
    /// Sockets listening on `port`, regardless of bind address.
    pub fn listeners_on_port(&self, port: u16) -> impl Iterator<Item = &ObservedSocket> {
        self.sockets
            .iter()
            .filter(move |s| s.is_listening() && s.local_port == port)
    }

    /// Established connections belonging to `pid`.
    pub fn connections_of(&self, pid: u32) -> impl Iterator<Item = &ObservedSocket> {
        self.sockets
            .iter()
            .filter(move |s| s.is_established() && s.owned_by(pid))
    }

    /// Distinct PIDs seen anywhere in the snapshot.
    pub fn owning_pids(&self) -> BTreeSet<u32> {
        self.sockets
            .iter()
            .flat_map(|s| s.pids.iter().copied())
            .collect()
    }
}

/// Platform-independent socket collection.
#[async_trait]
pub trait SocketCollector: Send + Sync {
    async fn snapshot(&self) -> Result<SocketSnapshot, CollectorError>;
}

/// `netstat2`-backed collector.
#[derive(Debug, Clone)]
pub struct Netstat2SocketCollector {
    address_families: AddressFamilyFlags,
    protocols: ProtocolFlags,
}

impl Default for Netstat2SocketCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Netstat2SocketCollector {
    /// IPv4 + IPv6, TCP + UDP.
    pub fn new() -> Self {
        Self {
            address_families: AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
            protocols: ProtocolFlags::TCP | ProtocolFlags::UDP,
        }
    }

    /// TCP only. Cheaper, and enough for the Milestone 0 topology question.
    pub fn tcp_only() -> Self {
        Self {
            address_families: AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
            protocols: ProtocolFlags::TCP,
        }
    }
}

#[async_trait]
impl SocketCollector for Netstat2SocketCollector {
    async fn snapshot(&self) -> Result<SocketSnapshot, CollectorError> {
        let address_families = self.address_families;
        let protocols = self.protocols;

        let snapshot = tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let captured_at = SystemTime::now();

            let raw = get_sockets_info(address_families, protocols)
                .map_err(|err| CollectorError::platform("socket", err))?;

            let mut sockets = Vec::with_capacity(raw.len());
            let mut sockets_without_owner = 0usize;

            for info in raw {
                let pids = info.associated_pids.clone();
                if pids.is_empty() {
                    sockets_without_owner += 1;
                }

                let socket = match info.protocol_socket_info {
                    ProtocolSocketInfo::Tcp(tcp) => {
                        let state = SocketState::from(tcp.state);
                        let connected = !matches!(state, SocketState::Listen | SocketState::Closed);
                        ObservedSocket {
                            protocol: Protocol::Tcp,
                            local_addr: tcp.local_addr,
                            local_port: tcp.local_port,
                            remote_addr: connected.then_some(tcp.remote_addr),
                            remote_port: connected.then_some(tcp.remote_port),
                            state,
                            pids,
                        }
                    }
                    ProtocolSocketInfo::Udp(udp) => ObservedSocket {
                        protocol: Protocol::Udp,
                        local_addr: udp.local_addr,
                        local_port: udp.local_port,
                        remote_addr: None,
                        remote_port: None,
                        state: SocketState::Stateless,
                        pids,
                    },
                };
                sockets.push(socket);
            }

            sockets.sort_unstable_by_key(|s| (s.local_port, s.remote_port, s.local_addr));

            Ok::<_, CollectorError>(SocketSnapshot {
                captured_at,
                duration: started.elapsed(),
                sockets,
                sockets_without_owner,
            })
        })
        .await
        .map_err(|source| CollectorError::Join {
            collector: "socket",
            source,
        })??;

        tracing::debug!(
            sockets = snapshot.sockets.len(),
            without_owner = snapshot.sockets_without_owner,
            duration_us = snapshot.duration.as_micros(),
            "socket snapshot"
        );

        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    #[tokio::test]
    async fn finds_a_listener_opened_by_this_process() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let own_pid = std::process::id();

        let snapshot = Netstat2SocketCollector::tcp_only()
            .snapshot()
            .await
            .expect("snapshot");

        let owned = snapshot
            .listeners_on_port(port)
            .find(|s| s.owned_by(own_pid));
        assert!(
            owned.is_some(),
            "listener on port {port} not attributed to pid {own_pid}; \
             saw {:?}",
            snapshot.listeners_on_port(port).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn listening_sockets_have_no_remote_endpoint() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("local addr").port();

        let snapshot = Netstat2SocketCollector::tcp_only()
            .snapshot()
            .await
            .expect("snapshot");

        for socket in snapshot.listeners_on_port(port) {
            assert_eq!(socket.remote_addr, None);
            assert_eq!(socket.remote_port, None);
        }
    }

    #[tokio::test]
    async fn duration_is_measured() {
        let snapshot = Netstat2SocketCollector::new()
            .snapshot()
            .await
            .expect("snapshot");
        assert!(snapshot.duration > Duration::ZERO);
        assert!(!snapshot.sockets.is_empty(), "a desktop always has sockets");
    }
}
