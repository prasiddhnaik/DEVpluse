# DevPulse Test Plan

## Principle

Do not test system discovery only against the developer's machine.

Create deterministic fixture processes.

## Fixture suite

### TCP server

Config:

```text
port
lifetime
optional response
```

### TCP client

Config:

```text
target
connection lifetime
```

### HTTP server

Routes:

```text
/health -> 200
/slow -> configurable delay
/fail -> 500
```

### Memory grower

Allocates configurable memory over time.

### CPU burner

Consumes configurable CPU.

### Restart fixture

Starts/exits repeatedly.

### Docker fixture

Compose stack:

```text
fixture-api
fixture-postgres
fixture-redis
```

## Unit tests

### Project resolver

Cases:

- nested Git repo
- monorepo
- Node app
- Rust app
- Python app
- unrelated system directory

### Service identity

Cases:

- PID changes
- same executable, different project
- same project, different ports
- Docker container restart

### Snapshot diff

Cases:

- start
- stop
- restart
- new port
- lost port
- connection start/end

### Correlation

Cases:

- file change before restart
- unrelated file change
- health failure after restart
- events outside correlation window

## Integration tests

### I1 Port ownership

1. launch fixture server
2. collect sockets
3. assert correct PID owns port

### I2 Local topology

1. launch server
2. launch client
3. collect sockets
4. assert connection exists where supported

### I3 Restart identity

1. start server
2. register service
3. stop
4. restart on same project/port
5. assert logical ServiceId preserved

### I4 Resource monitoring

1. launch memory grower
2. collect samples
3. assert memory increases

### I5 WebSocket

1. connect test client
2. launch fixture
3. assert live service event arrives

### I6 Docker

1. launch fixture Compose stack
2. collect Docker state
3. assert service labels/ports recognized

## Performance tests

Measure:

- process collector duration
- socket collector duration
- state diff duration
- WebSocket broadcast cost
- idle daemon CPU
- idle daemon memory

Keep results in benchmark notes.

## Security tests

Verify:

- daemon binds only to loopback
- environment values are not persisted
- secret-looking process arguments are redacted
- arbitrary filesystem reads are impossible through API
- browser origin policy blocks unrelated sites
