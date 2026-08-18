# Runscape for coding agents

Runscape observes what is already running on the machine: processes, ports,
projects, restarts, warnings. It does not start, stop, or patch anything.

The dashboard is for humans (`runscape serve` opens it on the same port).
Agents use `--headless`.

**UI: "local pulse worker" = the daemon (`runscape serve`).** Keep saying
*daemon* in code. Do not put "daemon" in user-facing dashboard strings.

## Bring it up

```bash
runscape serve --headless
```

Waits for the first snapshot, then prints one JSON line on stdout and keeps
serving on loopback:

```json
{"ok":true,"http":"http://127.0.0.1:2013","ws":"ws://127.0.0.1:2013/ws/v1","ui":"http://localhost:2013","now":"runscape now --port 2013"}
```

No "press ctrl-c" banner. No browser. The visual UI is still at `ui` if a
human wants it. Port is always loopback; change it with `--port` or
`RUNSCAPE_PORT`.

If something is already serving, skip this step and query it.

## Read the machine (cheap)

Default output is compact JSON — no sparklines, no platform notes, no evidence
arrays. `--json` pretty-prints.

```bash
runscape now                 # projects, warnings, last 20 events
runscape now --here          # only the project that encloses cwd
runscape status              # liveness + collector timings
runscape projects
runscape project <id>
runscape service <id>
runscape events --limit 20
runscape warnings
runscape context <event-id>  # what happened around that event (not causation)
```

If nothing is listening, stdout is `{"ok":false,"error":"…","hint":"runscape serve --headless --port …"}` and the process exits 2.

## Watch (cheaper than polling)

```bash
runscape watch
```

NDJSON frames. The initial snapshot is omitted unless you pass `--snapshot`.
Default types: `events`, `warnings_changed`, `services_changed`,
`topology_changed`.

```bash
runscape watch --types events,warnings_changed
```

## Rules

1. Treat CLI JSON as the only runtime truth. Do not infer health or topology.
2. A PID is not a service id. Use the `id` fields Runscape returns.
3. `context` reports adjacency (same service, same project, nearby in time). It
   never means "caused by".
4. Do not send `Origin` (curl/CLI already omit it). Do not call the API from a
   browser page that is not the dashboard.
5. Nothing is uploaded. History is local SQLite (`~/.runscape/runscape.db`)
   unless `--no-persistence`.

## One-shot scans (no daemon)

These hit the OS directly and skip identity, warnings, and history:

```bash
runscape scan-projects --json
runscape scan-processes --json --filter node
runscape resolve-project .
```

Prefer `runscape now --here` once a daemon is up: it is the same facts, grouped
and bounded.
