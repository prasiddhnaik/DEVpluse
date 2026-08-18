//! Integration tests for Milestone 0 discovery (`TEST_PLAN.md` I1 and I2).
//!
//! These run against real fixture processes with known ports and known PIDs, so
//! a pass means socket-to-PID attribution genuinely works on this platform —
//! not that whatever happened to be running looked plausible.

use std::process::Stdio;
use std::time::Duration;

use devpulse_discovery::{
    Netstat2SocketCollector, ProcessCollector, SocketCollector, SocketSnapshot,
    SysinfoProcessCollector,
};
use devpulse_fixtures::ready_field;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Fixture processes must announce readiness quickly; anything slower is a bug,
/// not a slow machine.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// A fixture child process that is killed when the test ends, pass or panic.
struct Fixture {
    child: Child,
    pid: u32,
    ready_line: String,
}

impl Fixture {
    async fn spawn(bin: &str, args: &[&str]) -> Fixture {
        let mut child = Command::new(bin)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|err| panic!("spawn {bin}: {err}"));

        let stdout = child.stdout.take().expect("piped stdout");
        let mut lines = BufReader::new(stdout).lines();

        let ready_line = tokio::time::timeout(READY_TIMEOUT, lines.next_line())
            .await
            .unwrap_or_else(|_| panic!("{bin} did not announce readiness in {READY_TIMEOUT:?}"))
            .expect("read stdout")
            .unwrap_or_else(|| panic!("{bin} exited before announcing readiness"));

        let pid = ready_field(&ready_line, "pid")
            .unwrap_or_else(|| panic!("no pid in readiness line: {ready_line}"))
            .parse()
            .expect("pid is numeric");

        Fixture {
            child,
            pid,
            ready_line,
        }
    }

    fn field(&self, key: &str) -> &str {
        ready_field(&self.ready_line, key)
            .unwrap_or_else(|| panic!("no {key} in readiness line: {}", self.ready_line))
    }

    /// Port the fixture bound or connected to.
    fn addr_port(&self, key: &str) -> u16 {
        self.field(key)
            .rsplit_once(':')
            .expect("addr has a port")
            .1
            .parse()
            .expect("port is numeric")
    }

    async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fixture-tcp-server")
}

fn client_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fixture-tcp-client")
}

async fn sockets() -> SocketSnapshot {
    Netstat2SocketCollector::tcp_only()
        .snapshot()
        .await
        .expect("socket snapshot")
}

/// Poll the socket collector until `predicate` holds, exactly as the daemon's
/// 1 s snapshot loop would.
///
/// This is not flake padding: on macOS a connection that is still sitting in
/// the listener's accept queue has no file descriptor in the server process, so
/// `libproc` cannot attribute it until `accept()` returns. Discovery is
/// eventually consistent with the kernel, and the tests say so.
async fn sockets_until<F>(what: &str, predicate: F) -> SocketSnapshot
where
    F: Fn(&SocketSnapshot) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = sockets().await;
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; last snapshot had {} sockets",
            snapshot.sockets.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// I1 — the PID that opened a port is the PID DevPulse reports.
#[tokio::test]
async fn i1_listening_port_is_attributed_to_the_owning_pid() {
    let server = Fixture::spawn(server_bin(), &["--port", "0", "--lifetime-secs", "30"]).await;
    let port = server.addr_port("addr");

    let snapshot = sockets().await;
    let listeners: Vec<_> = snapshot.listeners_on_port(port).collect();

    assert!(
        !listeners.is_empty(),
        "no listener observed on port {port}; collector saw {} sockets",
        snapshot.sockets.len()
    );
    assert!(
        listeners.iter().any(|s| s.owned_by(server.pid)),
        "port {port} was not attributed to fixture pid {}; observed {listeners:?}",
        server.pid
    );

    server.stop().await;
}

/// I2 — a local client/server pair is observed from both ends, with the correct
/// PID on each side. This is the evidence that DevPulse can build topology from
/// `observed_socket` without inference.
#[tokio::test]
async fn i2_local_connection_is_observed_from_both_ends() {
    let server = Fixture::spawn(server_bin(), &["--port", "0", "--lifetime-secs", "30"]).await;
    let port = server.addr_port("addr");

    let target = format!("127.0.0.1:{port}");
    let client = Fixture::spawn(client_bin(), &["--target", &target, "--hold-secs", "25"]).await;
    let client_port = client.addr_port("local");

    let snapshot = sockets_until("both ends of the fixture connection", |snapshot| {
        let server_side = snapshot
            .connections_of(server.pid)
            .any(|s| s.local_port == port && s.remote_port == Some(client_port));
        let client_side = snapshot
            .connections_of(client.pid)
            .any(|s| s.local_port == client_port && s.remote_port == Some(port));
        server_side && client_side
    })
    .await;

    let server_side = snapshot
        .connections_of(server.pid)
        .find(|s| s.local_port == port && s.remote_port == Some(client_port));
    let client_side = snapshot
        .connections_of(client.pid)
        .find(|s| s.local_port == client_port && s.remote_port == Some(port));

    let server_side = server_side.expect("guaranteed by sockets_until");
    let client_side = client_side.expect("guaranteed by sockets_until");
    assert!(server_side.is_loopback_pair() && client_side.is_loopback_pair());
    assert_eq!(server_side.local_port, client_side.remote_port.unwrap());
    assert_eq!(client_side.local_port, server_side.remote_port.unwrap());

    client.stop().await;
    server.stop().await;
}

/// A listening socket must never be reported with a remote endpoint, otherwise
/// the topology builder would invent an edge.
#[tokio::test]
async fn listener_never_reports_a_peer() {
    let server = Fixture::spawn(server_bin(), &["--port", "0", "--lifetime-secs", "20"]).await;
    let port = server.addr_port("addr");

    let snapshot = sockets().await;
    for socket in snapshot.listeners_on_port(port) {
        assert_eq!(
            socket.remote_addr, None,
            "listener gained a peer: {socket:?}"
        );
        assert_eq!(socket.remote_port, None);
    }

    server.stop().await;
}

/// Process discovery must see the fixture with its real metadata.
#[tokio::test]
async fn process_discovery_sees_fixture_metadata() {
    let server = Fixture::spawn(server_bin(), &["--port", "0", "--lifetime-secs", "20"]).await;

    let snapshot = SysinfoProcessCollector::new()
        .snapshot()
        .await
        .expect("process snapshot");
    let process = snapshot
        .by_pid(server.pid)
        .unwrap_or_else(|| panic!("fixture pid {} not in process table", server.pid));

    assert!(
        process.name.contains("fixture-tcp-server"),
        "unexpected process name: {}",
        process.name
    );
    assert_eq!(
        process.parent_pid,
        Some(std::process::id()),
        "fixture must be a child of the test process"
    );
    assert!(
        process
            .executable
            .as_ref()
            .is_some_and(|exe| exe.ends_with("fixture-tcp-server")),
        "executable not resolved: {:?}",
        process.executable
    );
    assert_eq!(
        process.cwd,
        std::env::current_dir()
            .ok()
            .and_then(|d| std::fs::canonicalize(d).ok()),
        "cwd of an own-user process must be readable"
    );
    assert!(process.memory_bytes > 0);
    assert!(process.start_time_epoch_secs > 0);

    server.stop().await;
}

/// Secrets on a command line must be redacted before the observation exists
/// (`AGENTS.md` rule 6). Verified end to end, against a real process.
#[tokio::test]
async fn secret_arguments_are_redacted_before_capture() {
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwx";
    let server = Fixture::spawn(
        server_bin(),
        &["--port", "0", "--lifetime-secs", "20", "--response", secret],
    )
    .await;

    let snapshot = SysinfoProcessCollector::new()
        .snapshot()
        .await
        .expect("process snapshot");
    let process = snapshot
        .by_pid(server.pid)
        .unwrap_or_else(|| panic!("fixture pid {} not in process table", server.pid));

    assert!(
        !process.command.iter().any(|arg| arg.contains(secret)),
        "secret survived capture: {:?}",
        process.command
    );
    assert!(
        process.command.iter().any(|arg| arg == "<redacted>"),
        "expected a redacted argument, got {:?}",
        process.command
    );

    server.stop().await;
}

/// Collector cost must stay inside the polling budget from `ARCHITECTURE.md`
/// (1s process / 1s socket snapshots). A 250 ms ceiling leaves ample headroom
/// while still catching a pathological regression on CI hardware.
#[tokio::test]
async fn collector_duration_is_within_the_polling_budget() {
    let processes = SysinfoProcessCollector::new();
    processes.snapshot().await.expect("warm-up");
    let process_snapshot = processes.snapshot().await.expect("process snapshot");
    let socket_snapshot = sockets().await;

    assert!(
        process_snapshot.duration < Duration::from_millis(250),
        "process collector took {:?}",
        process_snapshot.duration
    );
    assert!(
        socket_snapshot.duration < Duration::from_millis(250),
        "socket collector took {:?}",
        socket_snapshot.duration
    );
}
