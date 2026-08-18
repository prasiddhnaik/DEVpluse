# What Runscape does for a developer

Runscape is a local-only observer for the code you are already running on your
machine. You start it, you keep working, and it shows you which projects are
up, which processes are services, which ports they own, how they talk to each
other, and what happened in the seconds around a crash or a restart.

It does not sit in your app. It does not need an account. It does not start,
stop, or restart anything. It reads what the operating system already knows
about *your* processes and turns that into a live picture.

```text
you run `runscape serve`
        │
        ▼
  local pulse worker watches this Mac ~1× per second
        │
        ▼
  dashboard at http://localhost:2013  (same process)
```

This document is the product, end to end. Install commands live in `README.md`.
The HTTP contract lives in `docs/api-contract.md`. Agents should read
`docs/for-agents.md` and use `runscape serve --headless` plus `runscape now`.

**UI: "local pulse worker" = the local Runscape daemon (`runscape serve`).**
Developers should keep saying *daemon* in code, crates, and PRs. Do not put
"daemon" in user-visible dashboard strings; the UI says "local pulse worker"
(or a grammatical variant: "the local pulse worker", "Local pulse worker").

---

## The problem it is for

A typical local stack is several processes, a couple of ports, maybe Docker,
and a terminal full of logs. When something breaks you already know the
symptoms. What you usually do not know, without digging, is:

- which *project* that process belongs to
- whether it is the same service that was running five minutes ago (the PID
  changed)
- who is connected to whom
- whether a save, a restart, a port clash, or a crash loop came first

Runscape answers those from OS facts, not from configuration you have to keep
up to date.

---

## What you run

One process for humans, the same process without a browser for agents:

| Process | Command | Role |
| --- | --- | --- |
| Local pulse worker | `runscape serve` | Discovers the machine, serves API + visual UI on `http://localhost:2013` |
| Headless (agents) | `runscape serve --headless` | Same worker; JSON ready line; no browser |

`cd apps/web && bun dev` is only for changing the React app. It is not required
to *use* Runscape.

The dashboard never invents health, topology, or identity. If a fact is on
screen, the daemon said it.

You can skip the dashboard. The CLI answers one-shot questions without a
daemon, and talks to a running daemon in compact JSON (see `docs/for-agents.md`):

```bash
runscape scan-processes
runscape scan-sockets
runscape scan-projects
runscape resolve-project .
runscape capabilities
runscape bench
runscape serve --headless
runscape now --here
runscape watch
```

---

## What it shows you

### Projects

Processes are grouped by the repository or workspace they are running in,
resolved from each process's working directory.

You see a name, a root path, how confident the grouping is, how many services
are running, CPU, memory, health, and the most recent warning if there is one.

A project is not "whatever is in the current folder." It is "every service
whose working directory resolved to this root." Two checkouts of the same repo
are two projects.

### Services

A service is a *logical* thing that can restart: your API, your Next.js app,
a Python `http.server`, a Compose container.

It is **not** a PID. After a crash-and-respawn you still have the same
service, with a restart count. Identity comes from a fingerprint (project,
runtime, executable, working directory, primary port — or Compose labels for
containers).

Not every process in a repo is a service. A shell, `sleep`, `git`, `cargo`,
and `rustc` are dropped. Anything listening on a port counts. A portless
worker counts only after it has been alive for 30 seconds, so a build step
does not become a fake service.

Health is derived from liveness and restart behaviour (`healthy`, `degraded`,
`stopped`, `unknown`). There are no HTTP health probes in 0.1.

### Topology

Edges are reconstructed from the kernel's TCP tables. If process A has an
established connection to a port process B is listening on, Runscape draws
A → B.

Every edge carries:

- **evidence type** — `observed_socket` is the one you can trust as fact
- **confidence** — 1.0 for an observed socket
- **first seen / last seen**

Sharing a Docker network is **not** treated as "these containers talk." That
would be a guess. Host → container edges work through published ports.

Solid lines in the dashboard were observed. Dashed lines are inferred or below
full confidence. Hover an edge to read the evidence.

### Containers

If Docker is running, containers appear in the same graph as host processes.
Identity comes from Compose labels, so `docker compose up` recreating a
container does not mint a new service.

If Docker is absent, the daemon continues with host processes only and says
so on `/api/v1/status`. That is a normal state, not an error. `--no-docker`
skips the probe entirely.

### Events

A timeline of what changed, not a log dump:

- project detected
- service started / stopped / restarted
- port opened / closed
- connection started / ended
- health changed
- file changed (in a project that has running services)

A stop followed by a start within two seconds is collapsed to a restart, so a
dev-server reload does not look like two unrelated events.

### Warnings

Deterministic rules, not a model. They appear while the condition is true and
clear when it is not.

| Rule | Meaning |
| --- | --- |
| `restart_loop` | The same service restarted enough times in a short window to look like a crash loop, not an edit-save-reload |
| `cpu_spike` | Sustained high CPU |
| `memory_growth` | One-way memory growth, not a sawtooth |
| `health_failure` | Health moved to a bad state |
| `port_conflict` | Two claimants for the same port (blames the port, not one process) |

### What changed

Click an event. Runscape returns the events around it, each labelled with
*why* it is related:

- same service
- same project
- preceding file change
- temporal (near in time, nothing stronger)

It never says "this save caused that restart." You get ordering and adjacency.
The conclusion is yours.

Typical story it *can* surface: you saved `server.js` → the Node service
restarted ~4 seconds later → the restart event's context lists that save as
`preceding_file_change`.

---

## A session, from the developer's chair

1. Start `runscape serve`. The dashboard is http://localhost:2013.
2. Start your usual stack — Node, Python, Rust fixtures, Compose, whatever.
3. The home page lists projects, running vs total services, CPU, memory.
4. Open a project. You get a graph, a service list, warnings, a timeline.
5. Something flaps. A `restart_loop` or `health_failure` warning appears.
6. Open the restart event. See whether a file change sat just before it.
7. Open a service. See its ports, restarts, command line (already redacted).
8. Stop working. Kill the daemon. History is in `~/.runscape/runscape.db`
   unless you passed `--no-persistence`. Live PIDs are not restored on the
   next start — they would already be wrong.

The dashboard routes:

| URL | What you get |
| --- | --- |
| `/` | Every known project |
| `/projects/:id` | Graph, services, warnings, timeline |
| `/services/:id` | One service and its neighbourhood |

---

## What it does *not* do

- It does not manage processes. No start, stop, restart, or kill from the UI
  or the API.
- It does not clone Datadog, Docker Desktop, or Activity Monitor.
- It does not sniff packets or read HTTP bodies.
- It does not read environment-variable *values*.
- It does not upload anything. Nothing leaves the machine.
- It does not require an agent in your application.
- It does not claim causation.
- It does not invent facts the OS withheld (other users' processes, sockets
  it cannot attribute).

---

## Privacy and safety, in product terms

The daemon binds **loopback only** and refuses to start on any other address.
A browser that is not the dashboard (`http://localhost:2013`,
`http://127.0.0.1:2013`, or the Next.js dev ports on 3000) is rejected. The API
is GET-only; no route takes a filesystem path or a command.

Command-line arguments that look like secrets (`--api-key=…`, JWTs, `ghp_…`)
are replaced with `<redacted>` at capture time. The raw argv is never stored
and never served.

History is one SQLite file. Delete it and it is gone.

Loopback is not a vault: another process *on this Mac* can still talk to
`127.0.0.1:2013`. That is the local-dev model. A random website cannot.

---

## What it cannot see (honest limits)

Unprivileged, on macOS and Linux:

- processes owned by other users have no cwd / argv / executable, so they
  never join a project
- sockets owned by other users are omitted, not reported with a fake owner
- a connection is attributable to the accepting side only after `accept()`
  has returned, so the graph can lag by about one snapshot interval
- a brand-new portless worker takes up to 30 seconds to appear (so builds
  do not pollute the project)
- Docker is untested on a machine with no Docker daemon
- health is liveness, not an HTTP `/health` probe

`GET /api/v1/status` reports the platform capability matrix and how many
fields the last tick could not read. That page is the honest version of
"why is the view thin."

---

## Cost

The design constraint is that Runscape must not become the resource problem.
Collectors run on a ~1 second tick, on a blocking thread pool, with bounded
history. On a debug build with ~190 processes this machine saw roughly
0.8% CPU and 22 MB RSS; process collection 15–24 ms/tick, sockets 3–7 ms.

`--docker-stats` is off by default: Docker's stats endpoint needs two samples
and can cost about a second per batch.

---

## How the pieces map, if you open the repo

```text
crates/runscape-cli         the `runscape` binary
crates/runscape-server      daemon: tick loop, HTTP, WebSocket
crates/runscape-core        domain: project, service, identity, topology
crates/runscape-discovery   OS: processes, sockets, file watch
crates/runscape-docker      optional Docker inspection
crates/runscape-events      diffs → events, warnings, "what changed"
crates/runscape-storage     SQLite + retention
apps/web                    dashboard (Next.js)
```

Rust owns runtime truth. The web app owns pixels.

---

## Related documents

| File | When to read it |
| --- | --- |
| `README.md` | Install, flags, repo layout |
| `docs/api-contract.md` | Exact HTTP / WebSocket shapes |
| `docs/daemon-verification.md` | Live scenario that closed the local-server milestone |
| `docs/discovery-spike-results.md` | What this OS will and will not disclose |
| `AGENTS.md` | Rules the implementation is held to |
| `runscape-agent-pack/ARCHITECTURE.md` | Crate-level architecture |
| `runscape-agent-pack/DECISIONS.md` | Why those rules exist |
