# AGENTS.md

## Mission

Build Runscape as a reliable, low-overhead, local-first developer tool.

The initial platform priority is:

1. macOS
2. Linux
3. Windows later

## Agent rules

### 1. Work milestone by milestone

Do not attempt the entire product in one pass.

Follow `TASKS.md` in order.

Do not begin polished UI work until the process and socket discovery spikes are proven.

### 2. Verify before claiming completion

For every task:

1. implement
2. run formatter
3. run linter
4. run tests
5. run a real fixture or integration scenario
6. record the result

Never mark a task complete because the code merely compiles.

### 3. Do not invent OS capabilities

macOS, Linux, and Windows expose different process/socket metadata.

If information is unavailable:

- return `None`
- lower confidence
- document the limitation
- continue operating

Do not fabricate relationships.

### 4. Evidence is mandatory

Every service-to-service edge must include:

- evidence type
- confidence
- first seen
- last seen

Allowed evidence types:

```text
observed_socket
docker_network
configured
otel_span
inferred
```

An inferred edge must never be rendered as certain.

### 5. PID is ephemeral

A PID is a process instance identifier, not a logical service identifier.

A service that restarts with a new PID should normally preserve its service identity.

### 6. Local-first security

The daemon must:

- bind to loopback by default
- never upload runtime data
- never collect environment-variable values
- never capture HTTP bodies
- redact likely secrets in process arguments
- avoid arbitrary command execution in the MVP

### 7. Performance matters

Runscape must not become the resource problem.

Collectors must:

- use bounded channels
- avoid unbounded history
- use reasonable polling intervals
- avoid blocking the async runtime
- measure collector duration

### 8. Rust owns runtime truth

Rust owns:

- process discovery
- socket discovery
- port discovery
- project grouping
- service identity
- topology
- Docker inspection
- events
- warnings
- local persistence
- WebSocket/API runtime state

The T3 application owns presentation.

Do not duplicate authoritative runtime state in Next.js.

## Preferred stack

### Rust

- stable Rust
- Tokio
- Axum
- serde
- tracing
- sysinfo
- netstat2 or platform-specific socket collection if required
- Bollard for Docker
- SQLite
- sqlx or rusqlite

### Web

- Next.js
- TypeScript
- React
- tRPC where useful
- Tailwind
- a graph library only after the graph data contract is stable

## Repository target

```text
runscape/
├── apps/
│   └── web/
├── crates/
│   ├── runscape-cli/
│   ├── runscape-core/
│   ├── runscape-discovery/
│   ├── runscape-docker/
│   ├── runscape-events/
│   ├── runscape-server/
│   └── runscape-storage/
├── fixtures/
├── docs/
├── AGENTS.md
└── Cargo.toml
```

## Rust quality gates

Run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Avoid `unwrap()` in daemon paths unless the invariant is proven.

## Frontend quality gates

Run the project's configured equivalents of:

```bash
pnpm lint
pnpm typecheck
pnpm test
```

## Commit discipline

Prefer small commits aligned to one task.

Examples:

```text
feat(discovery): add process snapshot collector
feat(discovery): map TCP sockets to owning PIDs
feat(core): add stable service identity
test(fixtures): add local TCP client/server pair
```

## Stop conditions

Stop and document the problem instead of papering over it if:

- socket-to-PID mapping is unreliable on a target OS
- required privileges would make default installation unreasonable
- a proposed collector requires packet payload capture
- a design needs arbitrary shell execution
- resource cost becomes excessive

A documented limitation is better than fake correctness.
