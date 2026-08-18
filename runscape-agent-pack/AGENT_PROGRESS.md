# Runscape Agent Progress

Agents should update this file after completing a task or milestone.

## Current milestone

Milestone 3 — Local server

## Status

Milestone 0 complete. Milestone 1 complete (typed IDs, service fingerprint, project grouping).
Milestone 2 complete (snapshot loop, registry, topology, events, resources).

## Completed

### Milestone 0
- T0.1 Rust workspace (`crates/runscape-core`, `crates/runscape-discovery`,
  `crates/runscape-cli`, `fixtures/`)
- T0.2 Process snapshot spike (`runscape scan-processes`)
- T0.3 Socket ownership spike (`runscape scan-sockets`)
- T0.4 TCP fixtures (`fixture-tcp-server`, `fixture-tcp-client`)
- T0.5 Project root resolver (`runscape resolve-project`, `runscape scan-projects`)
- Milestone 0 exit gate report (`docs/discovery-spike-results.md`)

### Milestone 1
- T1.1 Core typed IDs (`ProjectId`, `ServiceId`, `ConnectionId`, `EventId`)
- T1.2 Stable service fingerprint (`ServiceFingerprint`)
- T1.3 Project grouping engine (`GroupingEngine`, `ProjectResolver`)

### Milestone 2
- T2.1 Snapshot loop (`SnapshotLoop` in `runscape-server`)
- T2.2 Live service registry (`ServiceRegistry`)
- T2.3 Topology builder (`TopologyBuilder`)
- T2.4 Snapshot diff (event derivation via `EventDeriver`)
- T2.5 Resource history (`ResourceHistory`)

## In progress

T2.6 (Snapshot persistence) — not started

## Blocked

None.

## Verification log

```text
2026-08-17
Task: T0.1–T0.5 and Milestone 0 exit gate
Result: pass
Commands:
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo run --release -p runscape-cli -- capabilities
  cargo run --release -p runscape-cli -- scan-processes --filter /private/tmp/runscape-spike
  cargo run --release -p runscape-cli -- scan-sockets --port 41011
  cargo run --release -p runscape-cli -- scan-projects
  cargo run --release -p runscape-cli -- bench --iterations 30
Tests: 47 passed, 0 failed
  - runscape-core        26 unit (project resolver, redaction)
  - runscape-discovery    9 unit (process, socket, platform)
  - runscape-fixtures     3 unit + 6 integration (I1 port ownership, I2 local
                          topology, listener has no peer, fixture metadata,
                          end-to-end secret redaction, collector budget)
  - runscape-cli          3 unit (rendering)
Notes:
  - Socket-to-PID accuracy 12/12 versus `lsof` ground truth, 0 sockets with an
    undisclosed owner.
  - Unprivileged coverage 12 of 15 kernel listening ports; the missing 3 are
    root-owned.
  - Timing (release, 569 processes): process p50 5.92 ms, socket p50 1.36 ms
    against a 1 s polling budget.
  - Three-runtime fixture project at /private/tmp/runscape-spike (Node 41010,
    Python 41011, Rust 41012) grouped into one project at confidence 0.95, with
    the node -> Python edge observed from both ends.

2026-08-17
Task: T2.1 Snapshot loop
Result: pass
Commands:
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test -p runscape-server
Tests: 13 passed, 0 failed
  - runscape-server 13 unit (snapshot loop, dto, security)
Notes:
  - Implemented SnapshotLoop in runscape-server with continuous process/socket collection
  - Integrates existing collectors (SysinfoProcessCollector, Netstat2SocketCollector)
  - Coordinates grouping, registry reconciliation, topology building, resource sampling
  - Default tick interval: 1 second per TASKS.md
  - Tests verify loop construction, config defaults, and tick execution
```

## Known platform limitations

macOS (measured):

1. Unprivileged runs see only the current user's sockets, cwd, argv, parent PID
   and uid. ~33% of processes disclose none of those. Executable paths are
   available for every process.
2. A connection queued in a listener's accept backlog has no descriptor in the
   server process, so server-side attribution lags until `accept()` returns —
   eventually consistent within roughly one snapshot interval.
3. Sockets owned by invisible processes are omitted entirely, never reported
   with a guessed owner.
4. CPU percentages require two refreshes; the first snapshot reports 0 and sets
   `cpu_warming_up`.
5. Polling at 1 s cannot observe connections shorter than the interval.
6. No sudo, entitlement, or kernel extension is required for any of the above.

Linux: not measured — no machine available. Expected limits are recorded in
`runscape_discovery::platform::capabilities()` but are unverified.

Windows: no implementation; out of MVP scope.

## Important discoveries

- `netstat2` (libproc on macOS, `/proc` on Linux) is sufficient for socket→PID
  attribution. No `lsof` shell-out and no packet capture are needed.
- PID reuse is a real hazard for joining the socket table to the process table,
  because the two are sampled at slightly different instants. Milestone 1's
  service identity must not rely on a bare PID join.
- Two project-resolution false positives were found and fixed by dogfooding:
  `node_modules` package manifests were being treated as projects, and
  group-level evidence was reporting a single process's cwd depth as a
  project-wide fact.
- macOS/BSD accepted sockets inherit `O_NONBLOCK` from their listener; the TCP
  server fixture had to reset it or connections were torn down before discovery
  could observe them.

## Next task

Milestone 1, T1.1 — add core typed IDs (`ProjectId`, `ServiceId`, `ConnectionId`,
`EventId`) and the `Project` / `Service` / `ProcessInstance` / `Endpoint` /
`Connection` / `Evidence` types.
