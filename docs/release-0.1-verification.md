# Release 0.1 verification

`AGENTS.md` rule 2 requires a real scenario, not a passing build. This is the
run that closed the release gate on macOS 25.5 (arm64), 2026-08-18, after
Milestones 0–7.

## Gates

```bash
cargo fmt --all --check                                  # clean
cargo clippy --workspace --all-targets -- -D warnings    # clean
cargo test --workspace                                   # 276 passed
cd apps/web && bun test                                  # 25 pass, 6 skip (live
                                                         # tests need the daemon)
```

The file-watcher test `a_saved_file_is_reported_under_its_project` times out
inside a sandbox that cannot deliver FSEvents; it passes on the real machine
(0.37s).

## Service filter, live

The pre-filter daemon counted `sleep`, `zsh`, `cargo`, `rustc`, `ld` as
services. A `sleep` that recurred in the same directory produced
`restart_loop: sleep restarted 6 times`. The filter in
`crates/runscape-core/src/service_filter.rs` is: a listening process is always
a service; anything else must not be an OS tool, must not be a compiler or
build driver, and must have been alive for 30 seconds.

Verified against a **fresh** `target/debug/runscape` (not a stale binary
holding port 7778). After a clean start:

```text
3 projects
  Runscape    6/6  healthy   this repo (runscape :7778, node :3000, bun, …)
  stack-tcp   2/2  healthy   fixture-tcp-server :41001 + fixture-tcp-client
  stack-py    1/1  healthy   Python :41003
```

Absent, as required: `sleep`, `zsh`, `tail`, `cargo`, `rustc`, `ld`,
`caffeinate`, `rust-analyzer`. `rust-analyzer` is in `BUILD_TOOLS` and stayed
out even though it is long-lived.

The 30-second bar is visible in the live run: `fixture-tcp-client` has no
listening port, so it appeared one tick after 30s of uptime, and the
client→server edge arrived with it.

## Stopped-service retention

A stopped listener is "my API is down" and stays until `MAX_STOPPED_SERVICES`.
A stopped host process that never listened is dropped after 90 seconds
(`STOPPED_PORTLESS_RETENTION`) — long enough for restart-loop detection,
short enough that a project card does not read `9/68`. Containers are never
aged out this way.

Project card health is the worst health among *running* services. A dead
helper no longer paints a live project as stopped.

## Three stacks

| Stack | How | Observed |
| --- | --- | --- |
| Rust fixtures | `fixture-tcp-server :41001` + client holding the socket | Own git project. Solid `observed_socket` edge, confidence 1.0, port 41001. |
| Python | `python3 -m http.server 41003` in a scratch git repo | Own git project, runtime `python`, listening on 41003. |
| This repo | `runscape serve` + Next.js `bun run dev` | Grouped as `Runscape`. Daemon on 7778, dashboard on 3000. |

A Node stack (`stack-node` on 41004) and a second Rust fixture pair were
verified in the same working tree earlier the same day, including secret
redaction (`--api-key=…` → `--api-key=<redacted>`) and the T7.2
file-save → restart → `preceding_file_change` story.

## Loopback and cost

`lsof` showed `TCP 127.0.0.1:7778 (LISTEN)` — not `*:7778`. Debug build, ~200
processes on the machine:

| Metric | Value |
| --- | --- |
| CPU | 0.1% |
| RSS | 22 MB |
| Process collector | 12–22 ms/tick |
| Socket collector | 3–9 ms/tick |
| SQLite after ~4 minutes, 3 projects | 204 KB |

An hours-long session was not run. Retention is bounded in code (event ring,
resource history, stopped-service cap); the four-minute database growth is
not a substitute for that run.

## Dashboard, looked at

`bun run dev` in `apps/web`, then `http://localhost:3000` in a real browser
(Playwright). Connected to the daemon. Three project cards, counts
`3 / 9 / 1`, docker shown as off with the socket-missing reason on hover.

The `stack-tcp` project page drew its own SVG: client → server, solid edge,
tooltip `observed_socket, confidence 1.00`. Service inspector showed the
redacted command, `127.0.0.1:41001`, inbound `observed_socket · 100%`, and a
CPU/memory sparkline.

Cosmetic: `favicon.ico` is 404. In `next dev`, React Strict Mode closes the
first WebSocket before it finishes connecting; the remount succeeds and the
badge reads `connected`. `next start` does not double-mount.

## Docker

This machine has no Docker daemon (`/var/run/docker.sock` does not exist).
`/status` reports `available: false` with that reason. M6 is covered by unit
tests and `crates/runscape-server/tests/containers.rs` (fake collector). It
has not run against a Compose stack (`fixture-api`, `fixture-postgres`,
`fixture-redis` in `TEST_PLAN.md` I6).

## OS limitations (carried from `/status` platform notes)

* Unprivileged macOS sees only the calling user's sockets and process
  metadata (`cwd`, argv, parent). Other users' processes degrade to `None`
  rather than being invented.
* A connection still in a listener's accept backlog has no server-side fd, so
  it is attributed to the client only until `accept()`. Topology is eventually
  consistent within one snapshot interval.
* No packet capture, no environment-variable collection, no HTTP bodies.

## Still open after 0.1

1. Hours-long session: confirm CPU, RSS and database size stay flat.
2. Real Docker: Compose fixture on a machine that has a daemon.
3. `favicon.ico` for the dashboard.
4. The Strict Mode WebSocket abort in `next dev` (harmless, noisy).
