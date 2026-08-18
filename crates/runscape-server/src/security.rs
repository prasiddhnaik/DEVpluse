//! Browser origin policy.
//!
//! The daemon binds to loopback, but loopback is not a security boundary
//! against a browser: any page the developer visits can issue requests to
//! `http://localhost:2013`. Without an origin check, a random website could
//! read the developer's process list.
//!
//! So: an allow-list, applied to both CORS preflight and the WebSocket upgrade
//! (`AGENTS.md` "strict browser origin handling").

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Origins permitted to talk to the daemon from a browser.
///
/// Non-browser clients (curl, the CLI, tests) send no `Origin` header at all
/// and are allowed: they are not subject to the confused-deputy problem this
/// list exists to prevent.
#[derive(Debug, Clone)]
pub struct OriginPolicy {
    allowed: Vec<String>,
}

impl Default for OriginPolicy {
    fn default() -> Self {
        Self::new(&[
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://localhost:2013",
            "http://127.0.0.1:2013",
        ])
    }
}

impl OriginPolicy {
    pub fn new(origins: &[&str]) -> Self {
        Self {
            allowed: origins.iter().map(|o| o.to_ascii_lowercase()).collect(),
        }
    }

    pub fn allow(mut self, origin: impl Into<String>) -> Self {
        self.allowed.push(origin.into().to_ascii_lowercase());
        self
    }

    /// Permit browsers that loaded the dashboard from this bind address.
    pub fn with_bind(self, addr: SocketAddr) -> Self {
        let port = addr.port();
        self.allow(format!("http://127.0.0.1:{port}"))
            .allow(format!("http://localhost:{port}"))
            .allow(format!("http://[::1]:{port}"))
    }

    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed
    }

    /// `None` means the request carried no `Origin` header.
    pub fn permits(&self, origin: Option<&str>) -> bool {
        match origin {
            None => true,
            Some(origin) => {
                let origin = origin.trim().to_ascii_lowercase();
                self.allowed.iter().any(|a| a == &origin)
            }
        }
    }
}

/// The daemon must never be reachable from another machine
/// (`ARCHITECTURE.md`: loopback bind only). This is enforced at the point of
/// binding rather than trusted from configuration.
pub fn is_loopback_bind(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Default bind address: `127.0.0.1:2013`. The dashboard is opened as
/// `http://localhost:2013`.
pub const DEFAULT_PORT: u16 = 2013;

pub fn default_bind_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_the_dashboard_origin() {
        let policy = OriginPolicy::default();
        assert!(policy.permits(Some("http://localhost:3000")));
        assert!(policy.permits(Some("http://127.0.0.1:3000")));
        assert!(policy.permits(Some("http://127.0.0.1:2013")));
        assert!(policy.permits(Some("http://localhost:2013")));
    }

    #[test]
    fn rejects_unrelated_sites() {
        let policy = OriginPolicy::default();
        assert!(!policy.permits(Some("https://evil.example")));
        assert!(!policy.permits(Some("http://localhost:3001")));
        assert!(
            !policy.permits(Some("http://localhost:3000.evil.example")),
            "suffix tricks must not pass"
        );
    }

    #[test]
    fn origin_matching_ignores_case_and_padding() {
        let policy = OriginPolicy::default();
        assert!(policy.permits(Some(" HTTP://LocalHost:3000 ")));
    }

    #[test]
    fn non_browser_clients_have_no_origin_and_are_allowed() {
        assert!(OriginPolicy::default().permits(None));
    }

    #[test]
    fn with_bind_allows_the_actual_port() {
        let addr: SocketAddr = "127.0.0.1:41234".parse().expect("addr");
        let policy = OriginPolicy::default().with_bind(addr);
        assert!(policy.permits(Some("http://127.0.0.1:41234")));
        assert!(policy.permits(Some("http://localhost:41234")));
        assert!(!policy.permits(Some("http://127.0.0.1:41235")));
    }

    #[test]
    fn default_bind_is_loopback() {
        let addr = default_bind_addr();
        assert!(is_loopback_bind(&addr));
        assert_eq!(addr.port(), DEFAULT_PORT);
    }

    #[test]
    fn wildcard_bind_is_rejected() {
        let addr: SocketAddr = "0.0.0.0:7778".parse().expect("addr");
        assert!(!is_loopback_bind(&addr));
    }
}
