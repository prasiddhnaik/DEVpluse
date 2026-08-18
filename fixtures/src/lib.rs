//! Deterministic fixture processes for DevPulse discovery tests.
//!
//! Discovery must not be tested only against whatever happens to be running on
//! a developer's machine (see `TEST_PLAN.md`). These binaries provide processes
//! with known ports, known lifetimes, and known command lines.
//!
//! Both fixtures print a single machine-readable readiness line to stdout
//! before doing anything else useful, so tests never need to sleep-and-hope:
//!
//! ```text
//! fixture-tcp-server: READY pid=1234 addr=127.0.0.1:41001
//! fixture-tcp-client: CONNECTED pid=1235 local=127.0.0.1:52144 remote=127.0.0.1:41001
//! ```

/// Readiness prefix printed by `fixture-tcp-server`.
pub const SERVER_READY_PREFIX: &str = "fixture-tcp-server: READY";

/// Readiness prefix printed by `fixture-tcp-client`.
pub const CLIENT_READY_PREFIX: &str = "fixture-tcp-client: CONNECTED";

/// Parse `key=value` pairs out of a fixture readiness line.
pub fn parse_ready_line(line: &str) -> Option<Vec<(&str, &str)>> {
    let (_prefix, rest) = line
        .split_once("READY")
        .or_else(|| line.split_once("CONNECTED"))?;
    Some(
        rest.split_whitespace()
            .filter_map(|token| token.split_once('='))
            .collect(),
    )
}

/// Extract a single field from a fixture readiness line.
pub fn ready_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    parse_ready_line(line)?
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_ready_line() {
        let line = "fixture-tcp-server: READY pid=1234 addr=127.0.0.1:41001";
        assert_eq!(ready_field(line, "pid"), Some("1234"));
        assert_eq!(ready_field(line, "addr"), Some("127.0.0.1:41001"));
        assert_eq!(ready_field(line, "missing"), None);
    }

    #[test]
    fn parses_client_ready_line() {
        let line =
            "fixture-tcp-client: CONNECTED pid=9 local=127.0.0.1:52144 remote=127.0.0.1:41001";
        assert_eq!(ready_field(line, "remote"), Some("127.0.0.1:41001"));
    }

    #[test]
    fn rejects_unrelated_output() {
        assert!(parse_ready_line("some other log line").is_none());
    }
}
