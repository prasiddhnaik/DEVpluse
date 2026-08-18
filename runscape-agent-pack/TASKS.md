# Runscape Implementation Tasks

Complete in order.

---

# Milestone 0 — Prove discovery

Do not build the dashboard yet.

## T0.1 Create Rust workspace

Create:

```text
crates/runscape-cli
crates/runscape-core
crates/runscape-discovery
fixtures/
```

Acceptance:

- workspace builds
- fmt/clippy/test pass

## T0.2 Process snapshot spike

Implement:

```bash
cargo run -p runscape-cli -- scan-processes
```

Print:

```text
pid
parent pid
executable
cwd
cpu
memory
start time
```

Acceptance:

- identifies Node, Rust, Python processes on macOS
- does not crash on inaccessible processes
- collector duration is logged

## T0.3 Socket ownership spike

Implement:

```bash
cargo run -p runscape-cli -- scan-sockets
```

Print listening sockets and active TCP connections with owning PID where available.

Acceptance:

- start fixture server on port 41001
- Runscape identifies the correct PID
- start fixture client
- Runscape sees client/server connection if OS APIs permit
- limitations documented

## T0.4 TCP fixtures

Create deterministic fixture binaries:

```text
fixture-tcp-server
fixture-tcp-client
```

Server listens on configurable localhost port.

Client connects and stays connected for configurable duration.

## T0.5 Project root resolver

Input:

```text
process cwd
```

Output:

```text
nearest git/workspace/project root
evidence
confidence
```

Recognize:

```text
.git
Cargo.toml
package.json
pnpm-workspace.yaml
pyproject.toml
compose.yml
docker-compose.yml
```

Acceptance:

- unit tests for nested monorepo/project cases

## Milestone 0 exit gate

Produce `docs/discovery-spike-results.md` containing:

- macOS findings
- Linux findings if available
- socket ownership accuracy
- permissions required
- collector timing
- known limitations

Do not continue if basic socket/process discovery is fundamentally unreliable.

---

# Milestone 1 — Domain model

## T1.1 Add core IDs and types

Implement typed IDs:

```text
ProjectId
ServiceId
ConnectionId
EventId
```

Add:

```text
Project
Service
ProcessInstance
Endpoint
Connection
Evidence
```

## T1.2 Stable service fingerprint

Implement and test a deterministic service identity strategy.

Tests:

- same service after PID restart
- two different apps using same runtime remain separate
- Docker identity does not collide with host process

## T1.3 Project grouping engine

Convert observations into project memberships.

Every membership includes evidence/confidence.

---

# Milestone 2 — Live state engine

## T2.1 Snapshot loop

Run process/socket collectors continuously.

## T2.2 Service registry

Maintain active logical services.

## T2.3 Topology builder

Map observed socket relationships to service edges.

## T2.4 Snapshot diff

Emit:

```text
service_started
service_stopped
service_restarted
port_opened
port_closed
connection_started
connection_ended
```

## T2.5 Resource samples

Track CPU/memory without unbounded storage.

---

# Milestone 3 — Local server

## T3.1 Axum server

Bind:

```text
127.0.0.1:2013
```

Dashboard: `http://localhost:2013`.

## T3.2 Status endpoint

Implement:

```text
GET /api/v1/status
```

## T3.3 Project/service endpoints

Implement required read APIs.

## T3.4 WebSocket

Implement:

```text
/ws/v1
```

Send initial snapshot + incremental events.

Acceptance:

- test client receives service start/stop changes

---

# Milestone 4 — T3 dashboard

Only start now.

## T4.1 Create web app

Set up Next.js + TypeScript + Tailwind.

## T4.2 Daemon connection state

Show:

```text
connected
reconnecting
disconnected
```

## T4.3 Projects overview

Cards:

```text
project name
service count
health
memory
recent warning
```

## T4.4 Project graph

Nodes:

```text
service name
port
health
CPU
memory
```

Edges:

```text
evidence type
confidence
```

Do not animate heavily yet.

## T4.5 Service inspector

Show:

```text
processes
cwd
command (redacted)
ports
resources
connections
recent events
```

## T4.6 Timeline

Show current project events in chronological order.

---

# Milestone 5 — Persistence

## T5.1 SQLite schema

Persist:

```text
projects
services
events
warnings
aliases
```

## T5.2 Retention

Implement bounded retention.

Do not retain second-by-second resource samples forever.

---

# Milestone 6 — Docker

## T6.1 Detect daemon

Gracefully support Docker absent/unavailable.

## T6.2 Containers

Collect:

```text
name
image
state
ports
compose labels
```

## T6.3 Normalize into Service

Docker and host processes must use the same logical graph interface.

## T6.4 Docker stats

Add CPU/memory if low overhead.

---

# Milestone 7 — What changed?

## T7.1 File watcher

Watch only known active project roots.

## T7.2 Restart correlation

Example:

```text
file changed
service restarted 2s later
health failed 5s later
```

Store correlation, not causation.

## T7.3 Warning rules

Implement deterministic rules for:

```text
restart loop
CPU spike
persistent memory growth
health failure
port conflict
```

## T7.4 Context view

Given an event, show related events within a time window.

---

# Release gate

Before tagging 0.1:

- test 3 real development stacks
- run daemon for at least one extended development session
- inspect CPU/memory cost
- verify secret redaction
- verify loopback-only server
- document OS limitations
