# Runscape MVP Specification

## MVP objective

Prove that Runscape can reconstruct a useful local development environment automatically.

Given:

```text
Next.js      :3000
Rust API     :8080
PostgreSQL   :5432
Redis        :6379
```

Runscape should discover enough information to render something like:

```text
Next.js :3000
    |
    v
API :8080
  |     |
  v     v
Postgres Redis
:5432   :6379
```

Only observed or defensibly inferred connections may appear.

## Required MVP capabilities

### Process discovery

Capture where available:

- PID
- parent PID
- executable
- command
- cwd
- CPU
- memory
- start time
- process state

### Port/socket discovery

Capture:

- protocol
- local address
- local port
- remote address
- remote port
- owning PID
- listen / connected state

### Project grouping

Use evidence such as:

1. explicit user override
2. Docker Compose project labels
3. Git repository root
4. workspace root
5. working-directory ancestry
6. parent/child process relationship

### Stable logical services

Represent a service separately from a process instance.

Example:

```text
Service: API
Process instance #1: PID 910
Process instance #2: PID 1044
```

A restart changes the process instance, not necessarily the service.

### Topology

Edges must store:

```text
source
target
evidence_type
confidence
first_seen
last_seen
```

### Resource monitoring

For each service:

- current CPU
- current memory
- uptime

### Events

Minimum:

```text
project_detected
service_started
service_stopped
service_restarted
port_opened
port_closed
connection_started
connection_ended
health_changed
resource_warning
file_changed
```

### Docker

Detect:

- running containers
- names
- image
- ports
- state
- Compose labels
- CPU / memory if practical

### UI

Required screens:

1. projects overview
2. project graph
3. service inspector
4. event timeline
5. diagnostics

## Not MVP

Do not implement yet:

- accounts
- cloud sync
- team features
- production monitoring
- Kubernetes
- request payload capture
- packet capture
- AI root-cause analysis
- IDE extensions
- browser extension
- full log platform
- OpenTelemetry collector
- agent-specific integrations

## Success criteria

The MVP must successfully dogfood at least 3 real stacks.

Suggested:

1. Next.js only
2. frontend + API + database
3. Docker Compose multi-service stack

For each stack verify:

- correct project grouping
- correct port ownership
- service identity survives restart
- at least one observed local relationship where applicable
- live updates appear in UI
