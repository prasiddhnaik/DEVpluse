# DevPulse handoff — 2026-08-18 (release gate)

Continuation of the earlier handoff from the same day. Working tree is still
uncommitted (repo HEAD is `542cbec`; the original session started at `1596b2f`).

## Where the project stands

All seven milestones are implemented. The release-gate **code and live checks
that this machine can do are done**. Write-up:
`docs/release-0.1-verification.md`.

| Item | State |
| --- | --- |
| M0–M7 | done |
| Service filter, live daemon | **verified this session** — no `sleep`/`zsh`/`cargo`/`rustc`/`ld`/`rust-analyzer` |
| Stopped-service accumulation | **fixed this session** — 90s ageing for portless stopped hosts; project health ignores stopped helpers |
| Dashboard visual | **done this session** — Playwright against `http://localhost:3000` |
| `docs/release-0.1-verification.md` | **written** |
| Hours-long session | still open |
| Real Docker | still open — no `/var/run/docker.sock` |

### Gates as of this session

```bash
cargo fmt --all --check                                  # clean
cargo clippy --workspace --all-targets -- -D warnings    # clean
cargo test --workspace                                   # 276 passed
cd apps/web && bun test                                  # 25 pass, 6 skip
```

Live `lib/live.test.ts` skips when nothing is on 7778. The daemon was up
during the browser pass; it had exited by the time bun test was re-run.

## What changed after the previous handoff

1. Live filter verification (the previous session talked to a stale binary).
   Kill with `pkill -f "debug/devpulse"`, not `-f "devpulse serve"`.
2. `STOPPED_PORTLESS_RETENTION` (90s) in `crates/devpulse-core/src/registry.rs`.
   Listeners and containers keep the 256 cap. Tests in that file.
3. `project_health` in `crates/devpulse-server/src/dto.rs` — running services
   decide the card; an empty running set with stopped leftovers is `stopped`.
4. `agentRules: false` in `apps/web/next.config.ts` so `next dev` stops
   writing `AGENTS.md` / `CLAUDE.md` into the app.

## Graph review (parent session)

The knowledge graph was empty; a full rebuild produced 817 nodes / 8,163 edges
across 50 files. Working-tree risk vs HEAD is 0.65. Canvas:
the Cursor canvases directory, `code-review-graph.canvas.tsx`.

`service_filter.rs` and `apps/web` are not in the graph (50 parsed files, Rust
plus SQL). Live verification above is the coverage for those. Hubs:
`SnapshotLoop.tick`, `ServiceRegistry.apply`. Retention tests exist in
`registry.rs` but are not linked as `tests_for(evict_stopped)`.

`.cursor/mcp.json` runs `uvx code-review-graph serve` as a local stdio server
(not Runlayer-managed). That is a shadow MCP.

## Background processes

Started for this verification; safe to kill:

```bash
pkill -f "debug/devpulse"
pkill -f fixture-tcp
pkill -f "http.server 41003"
pkill -f "next dev"
```

Scratch fixtures live in `/tmp/devpulse-release/{stack-py,stack-tcp}`.
History DB is `/tmp/devpulse-check.db`. Default `~/.devpulse/devpulse.db`
was not touched.

## Committing

Still nothing committed. Natural split is one commit per milestone plus one
for the service filter / stopped-service retention and one for docs.
