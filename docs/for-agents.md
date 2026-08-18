# DevPulse for coding agents

DevPulse observes what is already running on the machine: processes, ports,
projects, restarts, warnings. It does not start, stop, or patch anything.

The dashboard is for humans. Agents use the CLI in headless mode.

**UI: "local pulse worker" = the daemon (`devpulse serve`).** Keep saying
*daemon* in code. Do not put "daemon" in user-facing dashboard strings.

## Bring it up

```bash
devpulse serve --headless
```

Waits for the first snapshot, then prints one JSON line on stdout and keeps
serving on loopback:

```json
{"ok":true,"http":"http://127.0.0.1:7778","ws":"ws://127.0.0.1:7778/ws/v1","now":"devpulse now --port 7778"}
```

No dashboard. No "press ctrl-c" banner. Port is always loopback; change it with
`--port` or `DEVPULSE_PORT`.

If something is already serving, skip this step and query it.

## Read the machine (cheap)

Default output is compact JSON — no sparklines, no platform notes, no evidence
arrays. `--json` pretty-prints.

```bash
devpulse now                 # projects, warnings, last 20 events
devpulse now --here          # only the project that encloses cwd
devpulse status              # liveness + collector timings
devpulse projects
devpulse project <id>
devpulse service <id>
devpulse events --limit 20
devpulse warnings
devpulse context <event-id>  # what happened around that event (not causation)
```

If nothing is listening, stdout is `{"ok":false,"error":"…","hint":"devpulse serve --headless --port …"}` and the process exits 2.

## Watch (cheaper than polling)

```bash
devpulse watch
```

NDJSON frames. The initial snapshot is omitted unless you pass `--snapshot`.
Default types: `events`, `warnings_changed`, `services_changed`,
`topology_changed`.

```bash
devpulse watch --types events,warnings_changed
```

## Rules

1. Treat CLI JSON as the only runtime truth. Do not infer health or topology.
2. A PID is not a service id. Use the `id` fields DevPulse returns.
3. `context` reports adjacency (same service, same project, nearby in time). It
   never means "caused by".
4. Do not send `Origin` (curl/CLI already omit it). Do not call the API from a
   browser page that is not the dashboard.
5. Nothing is uploaded. History is local SQLite (`~/.devpulse/devpulse.db`)
   unless `--no-persistence`.

## One-shot scans (no daemon)

These hit the OS directly and skip identity, warnings, and history:

```bash
devpulse scan-projects --json
devpulse scan-processes --json --filter node
devpulse resolve-project .
```

Prefer `devpulse now --here` once a daemon is up: it is the same facts, grouped
and bounded.
