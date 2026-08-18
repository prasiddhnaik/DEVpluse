# DevPulse Architecture

## System

```text
┌──────────────────────────────────────┐
│         T3 / Next.js Dashboard       │
│ React · TypeScript · Tailwind        │
└──────────────────┬───────────────────┘
                   │
             HTTP / WebSocket
                   │
┌──────────────────▼───────────────────┐
│         devpulse-server (Rust)       │
│                Axum                  │
└──────────────────┬───────────────────┘
                   │
┌──────────────────▼───────────────────┐
│          devpulse-core (Rust)        │
│                                      │
│ project resolver                     │
│ service registry                     │
│ topology builder                     │
│ health engine                        │
│ warning/correlation engine           │
└───────┬─────────────┬───────────────┘
        │             │
┌───────▼──────┐ ┌────▼──────────────┐
│ discovery    │ │ events            │
│              │ │                   │
│ processes    │ │ snapshot diff     │
│ sockets      │ │ derived events    │
│ ports        │ │ correlations      │
│ file watcher │ │ warnings          │
└──────┬───────┘ └────┬──────────────┘
       │               │
┌──────▼───────┐ ┌─────▼─────────────┐
│ Docker       │ │ SQLite            │
│ Bollard      │ │ persistence       │
└──────────────┘ └───────────────────┘
```

## Crates

### devpulse-core

Pure domain layer.

No direct platform APIs.

Suggested types:

```rust
Project
Service
ProcessInstance
Endpoint
Connection
ResourceSample
HealthObservation
DevPulseEvent
Warning
Evidence
```

### devpulse-discovery

Platform-facing collectors.

Suggested traits:

```rust
#[async_trait]
pub trait ProcessCollector {
    async fn snapshot(&self) -> Result<Vec<ObservedProcess>>;
}

#[async_trait]
pub trait SocketCollector {
    async fn snapshot(&self) -> Result<Vec<ObservedSocket>>;
}
```

Keep macOS/Linux implementations isolated.

### devpulse-events

Converts snapshots into durable events.

Pipeline:

```text
snapshot N
   |
snapshot N+1
   v
diff
   v
raw changes
   v
debounce / normalize
   v
domain events
   v
warnings / correlations
```

### devpulse-server

Expose:

```text
GET /api/v1/status
GET /api/v1/projects
GET /api/v1/projects/:id
GET /api/v1/services/:id
GET /api/v1/events
GET /api/v1/graph/:projectId
WS  /ws/v1
```

### devpulse-storage

SQLite.

Persist:

- projects
- stable services
- recent events
- user aliases
- warnings
- limited resource history

## Polling starting point

These are starting values, not sacred constants:

```text
process snapshot     1s
socket snapshot      1s
resource stats       1s
Docker state         2s
health probes        5s
file changes         event-driven
```

Measure cost.

## Project resolution

Return:

```rust
struct ProjectMatch {
    project_id: ProjectId,
    confidence: f32,
    evidence: Vec<ProjectEvidence>,
}
```

Do not silently collapse low-confidence matches.

## Service identity

Do not key services by PID.

Possible fingerprint inputs:

```text
project
executable/runtime
cwd
primary listening port
container identity
compose service identity
```

The algorithm should be deterministic and unit-tested.

## Connection confidence

Examples:

```text
observed_socket  -> 1.00
otel_span        -> 1.00
docker_network   -> 0.80
inferred         -> lower and explain why
```

These numbers are initial policy and may change.

## Frontend state

The frontend receives:

1. initial snapshot
2. incremental WebSocket updates

On reconnect, request a fresh snapshot rather than relying on perfect event replay.

## Security defaults

- loopback bind only
- strict browser origin handling
- no arbitrary command execution
- no environment value capture
- no packet payload capture
- redact likely secrets from command arguments
