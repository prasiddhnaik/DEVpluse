# Milestone 3 verification — local daemon

`AGENTS.md` rule 2 requires a real scenario, not a passing build. This is the
run that closed Milestone 3, on macOS 25.5 (arm64), 2026-08-18.

## Gates

```bash
cargo fmt --all --check      # clean
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace       # 224 passed, 0 failed
```

New server tests: `tests/api.rs` (11, HTTP contract), `tests/websocket.rs`
(5, live socket), `tests/daemon.rs` (2, real snapshot loop + loopback bind).

## Live scenario

A scratch project (`git init` + `package.json`), a fixture TCP server on
41001, a fixture TCP client holding a connection, and the daemon:

```bash
devpulse serve --port 7788 --no-docker --interval 1
```

Observed through the API:

* `GET /api/v1/status` — `platform.os = macos`, process collector 24 ms,
  socket collector 11 ms, `sockets_without_owner: 0`.
* `GET /api/v1/projects` — the fixture project resolved as `git_repository`
  at confidence 0.95, four services, `health: healthy`.
* `GET /api/v1/graph/:id` — `fixture-tcp-server` carrying port 41001, and the
  client → server edge:

  ```json
  { "source": "svc_9fc667d6772f", "target": "svc_96d79938592f",
    "target_port": 41001,
    "evidence": { "evidence_type": "observed_socket", "confidence": 1.0,
                  "first_seen": "2026-08-18T08:54:21Z",
                  "last_seen": "2026-08-18T08:54:24Z", "detail": null } }
  ```

* `GET /api/v1/events` — when the fixture server reached its lifetime, the
  daemon emitted `port_closed`, then `health_changed healthy → stopped`, then
  `service_stopped`, in that order, all carrying the project id.

## Costs at 1 Hz

Collector wall time per tick was 24 ms (process) and 11 ms (socket) with ~190
processes on the machine — inside the 1 s budget with room to spare. The
process collector reported 183 processes with unreadable `cwd`, `command`,
`parent_pid` and `user`: those belong to other users, exactly as
`platform.process_cwd = same_user_only` predicts. Degraded counts are surfaced
in `/status` rather than hidden.

## Known gaps leaving Milestone 3

* Warnings are always empty — the rule engine is Milestone 7.
* Docker containers are probed for availability but not yet folded into the
  service graph; `devpulse-docker` has the collector, the snapshot loop does
  not call it yet (Milestone 6 wiring).
* SQLite persistence exists in `devpulse-storage` but the daemon keeps events
  in a 2000-entry memory ring only (Milestone 5 wiring).
* Health is derived from liveness only; there are no HTTP health probes.
