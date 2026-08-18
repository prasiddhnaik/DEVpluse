# Milestone 0 — Discovery Spike Results

Date: 2026-08-17
Verdict: **Milestone 0 exit gate is satisfied on macOS.** Process discovery,
socket-to-PID attribution, and project grouping are reliable enough to build the
domain model on. Proceed to Milestone 1.

## Test machine

| Item | Value |
| --- | --- |
| OS | macOS 26.5.2 (build 25F84) |
| Kernel | Darwin 25.5.0 |
| Arch | arm64 (Apple M3) |
| Rust | 1.96.0 (stable) |
| Privileges | ordinary user (uid 501), **no sudo, no entitlements** |
| Process table size during runs | 569–661 processes |

## What was built

| Crate | Contents |
| --- | --- |
| `crates/devpulse-core` | project-root resolver with evidence + confidence; secret redaction |
| `crates/devpulse-discovery` | `ProcessCollector` / `SocketCollector` traits, `sysinfo` and `netstat2` implementations, platform capability matrix |
| `crates/devpulse-cli` | `scan-processes`, `scan-sockets`, `scan-projects`, `resolve-project`, `bench`, `capabilities` |
| `fixtures` | `fixture-tcp-server`, `fixture-tcp-client`, integration tests I1/I2 |

Implementation choice: socket discovery uses `netstat2`, which calls `libproc`
(`proc_pidfdinfo`) on macOS and reads `/proc` on Linux. No shelling out to
`lsof`/`netstat`, no packet capture, no environment-variable collection.

## Process discovery (T0.2)

Node, Python, and Rust processes are all identified with full metadata when they
belong to the current user:

```text
    PID    PPID   CPU%       MEM   UPTIME  NAME                 CWD                               EXECUTABLE
  76444   71309    0.0      8.3M    4m31s  Python               /private/tmp/devpulse-spike/api   …/Python.app/Contents/MacOS/Python
  76466   71309    0.0     36.6M    4m24s  node                 /private/tmp/devpulse-spike/web   /opt/homebrew/bin/node
  76495   71309    0.0      1.2M    4m13s  fixture-tcp-server   /private/tmp/devpulse-spike       …/target/release/fixture-tcp-server
```

Inaccessible processes degrade instead of failing. On a typical run:

```text
569 processes total
undisclosed by OS: cwd 190 | exe 0 | cmd 190 | parent 190 | user 190
```

**~33% of processes disclose no cwd, argv, parent, or uid** — these are the
root-owned system daemons. Executable paths are readable for *all* processes,
including root's, because `proc_pidpath` is not uid-restricted. Missing fields
are reported as `None` and counted in `ProcessSnapshot::degradations`; nothing is
inferred to fill the gap.

CPU usage is a delta between two refreshes. The first snapshot always reports
`0.0`, and the snapshot carries `cpu_warming_up: true` so callers cannot mistake
a warm-up value for an idle process.

## Socket ownership accuracy (T0.3)

### Against `lsof` ground truth

Every listening TCP port that `lsof -nP -iTCP -sTCP:LISTEN` attributes to a PID
is attributed to the same PID by DevPulse:

```text
ports agreeing on PID: 12/12
PID mismatches:  none
in lsof only:    none
in devpulse only: none
sockets with undisclosed owner: 0
```

### Against the kernel's full socket table

`netstat -an -p tcp` sees 15 distinct listening ports; DevPulse (unprivileged)
sees 12. The three missing ports — 53, 8021, 49173 — are owned by other users'
(root) processes.

**Accuracy of what is reported: 100%. Coverage without root: 12/15 listening
ports (80%) on this machine.** Sockets belonging to invisible processes are
omitted entirely; they are never reported with a guessed or empty owner, so the
topology builder can treat any reported PID as trustworthy.

### Observed local topology (I2)

Fixture stack: Node service on 41010 connecting to a Python service on 41011.
Both ends of the loopback connection are observed with the correct owning PID:

```text
PROTO STATE         LOCAL                REMOTE               PID    PROCESS
tcp   listen        127.0.0.1:41011      -                    76444  Python
tcp   established   127.0.0.1:41011      127.0.0.1:63501      76444  Python
tcp   established   127.0.0.1:63501      127.0.0.1:41011      76466  node
```

This is a genuine `observed_socket` edge (confidence 1.00 per
`ARCHITECTURE.md`): node(76466) → Python(76444) on port 41011, derived from
kernel state on both sides, with no inference.

## Project grouping (T0.5)

Resolution policy, in order: nearest `.git`, then outermost workspace root, then
nearest package manifest, then nearest compose file. Directories that can never
be projects (`/`, `$HOME` and its ancestors, system prefixes) and vendored
dependency trees (`node_modules`, `site-packages`, `.venv`, `vendor`, `Pods`)
are rejected.

A three-runtime fixture project groups correctly from working-directory evidence
alone:

```text
devpulse-spike [git-repository] confidence 0.95
  root      /private/tmp/devpulse-spike
  processes Python(76444), node(76466), fixture-tcp-server(76495)
  listening 41010, 41011, 41012
  evidence  git root at /private/tmp/devpulse-spike/.git;
            node-workspace workspace at /private/tmp/devpulse-spike/package.json
```

Two false positives were found and fixed during the spike:

1. `~/node_modules/@scope/pkg` was reported as a project. Dependency trees are
   now excluded by path component.
2. Group-level evidence printed one arbitrary process's `cwd` depth as if it
   described the whole project. Working-directory depth is now reported per
   process only.

## Collector timing

`devpulse bench --iterations 30`, release build, 569 processes / 54 sockets:

| Collector | min | p50 | max | budget (`ARCHITECTURE.md`) | duty cycle at p50 |
| --- | --- | --- | --- | --- | --- |
| process (`sysinfo`) | 5.40 ms | 5.92 ms | 6.52 ms | 1 s | 0.6% |
| socket (`netstat2`) | 1.29 ms | 1.36 ms | 1.56 ms | 1 s | 0.14% |

With 661 processes the process collector measured 6.97 / 7.53 / 10.24 ms. Debug
builds cost roughly 2× (12.6 ms / 3.2 ms p50).

Both collectors run on `spawn_blocking`, so neither stalls the async runtime.
The integration suite asserts a 250 ms ceiling per snapshot to catch regressions.

## Permissions required

**None.** No sudo, no entitlement, no kernel extension, no TCC prompt was
required for anything above. Running as root would only widen *coverage* to
other users' processes and sockets; it does not change accuracy. DevPulse should
ship unprivileged by default and treat the root-owned slice as invisible rather
than requesting elevation.

## Known limitations (macOS)

1. **Same-user visibility only.** Unprivileged runs cannot see other users' or
   root's sockets, cwd, argv, parent, or uid. Affected fields are `None`;
   affected sockets are absent.
2. **Accept-queue delay.** A connection still sitting in a listener's backlog
   has no file descriptor in the server process, so `libproc` attributes it to
   the client only until `accept()` returns. Server-side attribution is
   eventually consistent, typically within one snapshot interval. Integration
   test I2 polls for this rather than pretending it is instantaneous.
3. **PID reuse.** The socket table and the process table are sampled at slightly
   different instants; a PID recycled between the two samples could mis-attach a
   socket. Milestone 1's stable service identity (start time + project + port)
   must not trust a bare PID join.
4. **Ephemeral connections are missed.** With 1 s polling, connections shorter
   than the interval are never observed. This is a sampling floor, not a bug;
   short-lived HTTP calls will not appear as edges.
5. **CPU needs two samples.** Documented above; the first snapshot's CPU figures
   are zero by construction.
6. **`.git` heuristics.** Nested repositories are treated as separate projects,
   which is correct for vendored checkouts but will split a submodule from its
   superproject. Revisit if it causes friction in real stacks.

## Linux findings

**Not measured.** No Linux machine was available for this spike. The abstraction
is in place (`netstat2` reads `/proc/net/*` and `sysinfo` reads `/proc`), and
`platform::capabilities()` records the expected Linux limits — socket listing is
uid-independent but inode→PID mapping requires reading `/proc/<pid>/fd`, so PID
attribution is still same-user-only, and `hidepid=2` hides other users'
processes entirely. These are documented expectations from the API contracts,
**not verified observations**, and must be re-run on Linux before any Linux
support claim is made.

Windows is out of scope for the MVP and has no implementation.

## Security posture verified

- No environment variables are collected: `ProcessRefreshKind` explicitly
  excludes `environ`.
- Command lines are redacted **at capture time**, so raw argv never reaches
  storage or an API. Verified end to end against a live process with a
  `ghp_…`-shaped argument.
- No packet payload capture, no arbitrary command execution, no network I/O.

## Reproducing

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 47 tests

cargo run --release -p devpulse-cli -- capabilities
cargo run --release -p devpulse-cli -- scan-processes --filter node
cargo run --release -p devpulse-cli -- scan-sockets --listening
cargo run --release -p devpulse-cli -- scan-projects
cargo run --release -p devpulse-cli -- bench --iterations 30
```

Fixture-based verification:

```bash
cargo test -p devpulse-fixtures --test discovery
```

## Gate decision

| Exit criterion | Result |
| --- | --- |
| Process discovery works on macOS | yes, with documented same-user limits |
| Socket-to-PID mapping is reliable | yes — 12/12 agreement with `lsof`, 0 unattributed |
| Local client/server topology observable | yes, both ends, correct PIDs |
| Project grouping from cwd | yes, evidence + confidence on every match |
| Collector cost acceptable | yes, <1% duty cycle at 1 s polling |
| Requires unreasonable privileges | no |

Nothing in `AGENTS.md`'s stop-condition list was triggered. Milestone 1 (domain
model) may begin.
