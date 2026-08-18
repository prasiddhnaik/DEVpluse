# DevPulse local API contract (v1)

The daemon is the single source of runtime truth (`AGENTS.md` rule 8). The
dashboard renders it and never recomputes it.

## Transport and security

| Property | Value |
| --- | --- |
| Bind address | `127.0.0.1:7778`, loopback only, never `0.0.0.0` |
| Scheme | `http` / `ws` |
| Auth | none — reachability is the boundary |
| CORS | allow-list only: `http://localhost:3000`, `http://127.0.0.1:3000` |
| Methods | `GET` only. No mutating endpoint exists in the MVP (`DECISIONS.md` D004) |

No endpoint accepts a filesystem path, a command, or anything else that could
turn the API into a file reader or an executor.

## Common shapes

Timestamps are RFC 3339 UTC strings. Durations are integers in milliseconds.
`confidence` is a float in `0.0..=1.0`.

```jsonc
// Evidence — attached to every edge and every project membership
{
  "evidence_type": "observed_socket", // observed_socket | docker_network | configured | otel_span | inferred
  "confidence": 1.0,
  "first_seen": "2026-08-17T10:00:00Z",
  "last_seen":  "2026-08-17T10:04:31Z",
  "detail": null                      // required (non-null) when evidence_type = "inferred"
}
```

Errors:

```jsonc
{ "error": { "code": "not_found", "message": "unknown project prj_ab12cd34ef56" } }
```

`code` is one of `not_found`, `bad_request`, `unavailable`.

## `GET /api/v1/status`

Daemon liveness and self-reported capability. Cheap; safe to poll.

```jsonc
{
  "version": "0.1.0",
  "started_at": "2026-08-17T09:58:02Z",
  "uptime_ms": 273000,
  "platform": {
    "os": "macos",
    "process_list": "full",
    "process_cwd": "same_user_only",
    "socket_owner_pid": "same_user_only",
    "root_widens_view": true,
    "notes": ["…"]
  },
  "docker": { "available": false, "reason": "Cannot connect to the Docker daemon" },
  "counts": { "projects": 3, "services": 11, "connections": 4, "events": 512 },
  "collectors": {
    "process": { "last_duration_ms": 6, "last_run": "2026-08-17T10:02:35Z", "degraded_fields": { "cwd": 190 } },
    "socket":  { "last_duration_ms": 1, "last_run": "2026-08-17T10:02:35Z", "sockets_without_owner": 0 }
  }
}
```

## `GET /api/v1/projects`

```jsonc
[
  {
    "id": "prj_ab12cd34ef56",
    "name": "devpulse-spike",
    "root": "/private/tmp/devpulse-spike",
    "kind": "git_repository",       // git_repository | workspace | package | compose_stack
    "confidence": 0.95,
    "first_seen": "2026-08-17T09:58:10Z",
    "last_seen":  "2026-08-17T10:02:35Z",
    "service_count": 3,
    "running_service_count": 3,
    "health": "healthy",            // healthy | degraded | stopped | unknown  (worst of its services)
    "memory_bytes": 46137344,
    "cpu_percent": 1.4,
    "recent_warning": null          // most recent Warning, or null
  }
]
```

## `GET /api/v1/projects/:id`

The project, its services (same shape as `/services/:id`), its edges, and its
most recent events.

```jsonc
{
  "project": { /* as above */ },
  "services": [ /* Service */ ],
  "connections": [ /* Connection */ ],
  "warnings": [ /* Warning */ ],
  "recent_events": [ /* Event, newest first, capped at 100 */ ]
}
```

`404` with `code: "not_found"` for an unknown id.

## `GET /api/v1/services/:id`

```jsonc
{
  "id": "svc_1122aabbccdd",
  "project_id": "prj_ab12cd34ef56",
  "name": "web",
  "kind": { "kind": "host_process" },   // or { "kind": "container", "name": "...", "compose_project": "...", "compose_service": "..." }
  "runtime": "node",                     // node | bun | deno | python | rust | go | java | ruby | php | dotnet | container | native
  "fingerprint": "host|prj_ab12cd34ef56|node|node|/private/tmp/devpulse-spike/web|41010",
  "health": "healthy",
  "restart_count": 0,
  "first_seen": "2026-08-17T09:58:10Z",
  "last_seen":  "2026-08-17T10:02:35Z",
  "instances": [
    {
      "pid": 76466,
      "parent_pid": 71309,
      "executable": "/opt/homebrew/bin/node",
      "command": ["node", "server.js", "--api-key", "<redacted>"],
      "cwd": "/private/tmp/devpulse-spike/web",
      "started_at": "2026-08-17T09:58:10Z",
      "cpu_percent": 0.4,
      "memory_bytes": 38400000
    }
  ],
  "endpoints": [ { "address": "127.0.0.1", "port": 41010, "protocol": "tcp", "pid": 76466 } ],
  "resource_history": [ { "at": "2026-08-17T10:02:35Z", "cpu_percent": 0.4, "memory_bytes": 38400000 } ],
  "connections": { "outbound": [ /* Connection */ ], "inbound": [ /* Connection */ ] },
  "recent_events": [ /* Event, newest first, capped at 50 */ ]
}
```

`command` is **always** already redacted; the daemon never holds raw argv.

## `GET /api/v1/graph/:projectId`

Exactly what the graph view needs, nothing more.

```jsonc
{
  "project_id": "prj_ab12cd34ef56",
  "nodes": [
    { "id": "svc_1122aabbccdd", "name": "web", "runtime": "node", "health": "healthy",
      "port": 41010, "cpu_percent": 0.4, "memory_bytes": 38400000, "kind": "host_process" }
  ],
  "edges": [
    { "id": "con_998877665544", "source": "svc_1122aabbccdd", "target": "svc_ffeeddccbbaa",
      "target_port": 41011,
      "evidence": { "evidence_type": "observed_socket", "confidence": 1.0,
                    "first_seen": "…", "last_seen": "…", "detail": null } }
  ]
}
```

The UI must render `confidence < 1.0` or `evidence_type = "inferred"` visibly
differently from an observed edge (`AGENTS.md` rule 4).

## `GET /api/v1/events`

Query parameters: `project_id` (optional), `service_id` (optional),
`limit` (default 100, max 1000), `since` (RFC 3339, optional).
Newest first.

```jsonc
[
  {
    "id": "evt_0193f2a1b3c8000001",
    "at": "2026-08-17T10:02:35Z",
    "project_id": "prj_ab12cd34ef56",
    "kind": {
      "type": "service_restarted",   // see EventKind in devpulse-core
      "service_id": "svc_1122aabbccdd",
      "old_pid": 76466,
      "new_pid": 76901
    }
  }
]
```

## `WS /ws/v1`

The socket is the live channel; HTTP is the fallback and the cold start.

1. On connect the server sends exactly one `snapshot` frame.
2. Then it sends incremental frames as things change.
3. On reconnect the client requests a fresh snapshot rather than trusting event
   replay (`ARCHITECTURE.md`).

Server → client frames:

```jsonc
{ "type": "snapshot", "at": "…", "status": { /* /status body */ },
  "projects": [ /* project summaries */ ], "services": [ /* Service */ ],
  "connections": [ /* Connection */ ], "warnings": [ /* Warning */ ] }

{ "type": "events",  "at": "…", "events": [ /* Event */ ] }
{ "type": "services_changed", "at": "…", "services": [ /* full Service objects that changed */ ],
  "removed": ["svc_…"] }
{ "type": "topology_changed", "at": "…", "project_id": "prj_…",
  "added": [ /* Connection */ ], "removed": ["con_…"] }
{ "type": "warnings_changed", "at": "…", "warnings": [ /* Warning */ ], "removed": ["…"] }
```

Client → server frames: only `{ "type": "resnapshot" }`, which triggers a fresh
`snapshot` frame. Anything else is ignored.

Rules:

- Slow consumers are dropped rather than buffered without bound; the client
  reconnects and gets a fresh snapshot.
- Frames are JSON text, one JSON value per frame.
- The `Origin` header is checked against the CORS allow-list before upgrade.

## `GET /api/v1/warnings`

Every currently active warning, newest activity first. Optional `project_id`
filter. Warnings come from the deterministic rules in `devpulse-events`
(`TASKS.md` T7.3) and clear as soon as their condition stops being true.

```jsonc
[
  {
    "id": "warn_restart_loop_svc_1122aabbccdd",
    "rule": "restart_loop",          // restart_loop | cpu_spike | memory_growth | health_failure | port_conflict
    "severity": "critical",          // info | warning | critical
    "project_id": "prj_ab12cd34ef56",
    "service_id": "svc_1122aabbccdd",
    "message": "web restarted 3 times in the last 60s",
    "first_seen": "2026-08-17T10:02:35Z",
    "last_seen":  "2026-08-17T10:03:35Z",
    "related_events": ["evt_0193f2a1b3c8000001"]
  }
]
```

`port_conflict` has no `service_id`: a conflict is about the port, and blaming
one of its two claimants would be a guess.

## `GET /api/v1/events/:id/context`

What happened around one event (`TASKS.md` T7.4). Optional `window_ms`
(default 30000, clamped to `1000..=600000`).

```jsonc
{
  "event": { /* Event */ },
  "window_ms": 30000,
  "before": [
    {
      "id": "evt_…", "at": "…", "project_id": "prj_…", "kind": { "type": "file_changed", … },
      "relation": "preceding_file_change",  // same_service | same_project | preceding_file_change | temporal
      "offset_ms": -2000                    // negative is before the anchor
    }
  ],
  "after": [ /* same shape, offset_ms positive */ ]
}
```

`relation` says why two events are near each other. It is never "caused by":
DevPulse reports ordering and lets the developer draw the conclusion
(`DECISIONS.md` D008).

`404` with `code: "not_found"` when the event has fallen out of the daemon's
in-memory ring.

## Implementation status (0.1)

The routes above are served by `devpulse-server` and exercised by
`crates/devpulse-server/tests/`. Where the running daemon is narrower than this
document, it is narrower on purpose:

| Area | Status |
| --- | --- |
| `/status` `collectors.container` | Present only when the daemon has a Docker collector. Its `error` field says why a collection produced nothing. |
| Container services | Included in the registry, the graph and the events, identified by Compose labels. Their `instances` are always empty and event `pid`s are `null`: Docker does not disclose the host PIDs of a container's processes. |
| Container-to-container edges | Not drawn. Sharing a Docker network is not evidence that two containers talk, and inventing an edge would break `AGENTS.md` rule 4. Host-to-container edges appear normally, through published ports. |
| `warnings` | Live. Empty until a rule fires. |
| Event history | The in-memory ring holds the newest 2000 events and is restored from SQLite at startup. Older history stays in the database and is subject to retention (24h / 50k events by default). |
| Persisted services | Written, but never restored into the live view: a stored service carries PIDs that were true before the daemon stopped (`AGENTS.md` rule 5). |
| Disallowed `Origin` | `403` with body `{"error":{"code":"bad_request", …}}`. The daemon refuses to answer rather than relying on the browser to discard the response. |
| `limit` on `/api/v1/events` | Clamped to `1..=1000` rather than rejected. |
| `since` on `/api/v1/events` | UTC RFC 3339 only (`…Z`, fractional seconds truncated). An offset such as `+01:00` is a `bad_request`. |

Frames are broadcast to a bounded channel (256). A client that falls behind is
closed rather than buffered, and is expected to reconnect and re-snapshot.
