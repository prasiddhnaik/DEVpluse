# DevPulse

Run one command and see what your local code is already doing — the processes,
the ports, the containers, how they talk to each other, and what changed just
before something broke.

```bash
devpulse serve          # the daemon: discovery + local API on 127.0.0.1:7778
devpulse serve --headless   # same, JSON ready line, no dashboard needed
cd apps/web && bun dev  # the dashboard: http://localhost:3000
```

No configuration, no agent to install in your app, no account. DevPulse reads
what the operating system already knows about your own processes and groups it
into projects.

What it does for you, end to end — projects, services, topology, warnings,
“what changed”, safety limits — is in [`docs/for-developers.md`](docs/for-developers.md).
Coding agents should use [`docs/for-agents.md`](docs/for-agents.md).

## What it shows

* **Projects** — processes grouped by the repository or workspace they run in,
  resolved from each process's working directory, with the evidence and a
  confidence for the grouping.
* **Services** — a stable identity per logical service that survives a restart,
  because a PID does not (`DECISIONS.md` D006).
* **Topology** — service-to-service edges reconstructed from the kernel's socket
  tables. Every edge carries its evidence type, confidence, and when it was
  first and last seen.
* **Containers** — Docker containers appear as services in the same graph,
  identified by their Compose labels so `docker compose up` does not create a
  new identity.
* **Events and warnings** — starts, stops, restarts, ports, health changes and
  file changes, plus deterministic rules for restart loops, sustained CPU,
  one-way memory growth, degraded health and port conflicts.
* **What changed** — click any event to see what happened around it, labelled
  with *why* it is related. DevPulse states adjacency; it never claims cause.

## What it is not

Not a Datadog clone, not a Docker Desktop clone, not a process manager, not a
packet sniffer. It observes and reports; it starts and stops nothing
(`DECISIONS.md` D004).

## Install and run

Requires a stable Rust toolchain (1.85+) and, for the dashboard, Bun.

```bash
cargo build --release
./target/release/devpulse serve
```

Useful flags:

| Flag | Meaning |
| --- | --- |
| `--port <n>` | Listen on another port. The address is always loopback. |
| `--interval <secs>` | Snapshot interval. Default 1s. |
| `--db <path>` | History database. Default `~/.devpulse/devpulse.db`. |
| `--no-persistence` | Keep history in memory only; write nothing to disk. |
| `--no-docker` | Skip the Docker probe entirely. |
| `--docker-stats` | Per-container CPU/memory. Costs ~1s per snapshot batch. |
| `--headless` | Wait for the first snapshot, print one JSON ready line, serve without a dashboard. For agents: [`docs/for-agents.md`](docs/for-agents.md). |

The CLI also answers questions without the daemon:

```bash
devpulse scan-processes      # what is running, with cwd, CPU, memory
devpulse scan-sockets        # listening sockets and connections, with owning PIDs
devpulse scan-projects       # how processes group into projects
devpulse resolve-project .   # which project root a directory resolves to, and why
devpulse capabilities        # what this OS will and will not disclose
devpulse bench               # collector cost against the polling budget

# against a running daemon (compact JSON, for agents)
devpulse now --here          # this repo's projects, warnings, recent events
devpulse watch               # NDJSON event/warning/service frames
```

## The dashboard

```bash
cd apps/web
bun install
bun dev
```

It connects to `ws://127.0.0.1:7778/ws/v1`, renders what the daemon sends, and
computes no runtime facts of its own (`AGENTS.md` rule 8). Point it at another
daemon with `NEXT_PUBLIC_DEVPULSE_HTTP` and `NEXT_PUBLIC_DEVPULSE_WS`. The
dashboard UI calls this process the **local pulse worker**; keep saying *daemon*
in code, and never put "daemon" in user-visible strings. See the glossary in
[`docs/for-developers.md`](docs/for-developers.md).

## Privacy and security

* The daemon binds loopback only, and refuses to start on any other address.
* Browser requests are checked against an origin allow-list before they are
  answered, so a random website cannot read your process list.
* Environment variable *values* are never read.
* Secret-looking command-line arguments are redacted at capture time — the raw
  argv never reaches memory the API can serve.
* Nothing is uploaded. History lives in one SQLite file you can delete.
* No endpoint accepts a path or a command, so the API cannot be turned into a
  file reader or an executor.

## What it cannot see

DevPulse never invents a fact the OS did not give it (`AGENTS.md` rule 3).
Unprivileged on macOS and Linux, that means:

* processes owned by other users disclose no cwd, argv or executable, so they
  are not grouped into projects;
* sockets owned by other users are invisible rather than reported with an
  unknown owner;
* a connection is only attributable to the accepting side once `accept()` has
  returned, so topology is eventually consistent within about one interval.

`GET /api/v1/status` reports the current platform's capability matrix and how
many fields the last collection could not read. See
`docs/discovery-spike-results.md` for the measurements behind this.

## Repository layout

```text
crates/devpulse-core        domain model, identity, grouping, registry, topology
crates/devpulse-discovery   processes, sockets, file watching, platform limits
crates/devpulse-events      snapshot diffing, warning rules, correlation
crates/devpulse-docker      Docker inspection via Bollard
crates/devpulse-storage     SQLite persistence and retention
crates/devpulse-server      the daemon: snapshot loop, HTTP + WebSocket API
crates/devpulse-cli         the `devpulse` binary (scans, daemon, headless agent CLI)
apps/web                    the dashboard (Next.js, TypeScript, Tailwind)
fixtures                    deterministic TCP fixtures used by the tests
docs                        API contract, spike results, verification records
```

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd apps/web && bun run lint && bun run typecheck && bun test
```

`apps/web`'s test suite includes a contract check against a *running* daemon; it
skips itself when nothing is listening on 7778, and runs when there is.
