# Initial Agent Prompt

Use this prompt to begin implementation with Codex, Claude Code, or another repository agent.

---

You are starting implementation of **Runscape**.

Runscape is a local-first developer runtime discovery tool. Its goal is to automatically discover local development projects, processes, ports, resource usage, and service-to-service socket relationships, then expose that state to a live T3 dashboard.

Read these files before changing code:

1. `START_HERE.md`
2. `AGENTS.md`
3. `SPEC.md`
4. `ARCHITECTURE.md`
5. `TASKS.md`
6. `TEST_PLAN.md`
7. `DECISIONS.md`

Your task is **Milestone 0 only**.

Do not build the dashboard yet.

Implement the technical discovery spikes in this order:

1. create the Rust workspace
2. implement process discovery
3. create deterministic TCP server/client fixtures
4. implement socket/port ownership discovery
5. implement project-root resolution
6. add tests
7. write `docs/discovery-spike-results.md` with actual findings

Requirements:

- macOS is the first-class initial target
- Linux support should remain possible through abstractions
- unavailable process/socket metadata must degrade gracefully
- do not fabricate topology
- do not use packet payload capture
- do not collect environment-variable values
- measure collector duration
- run `cargo fmt --check`
- run `cargo clippy --workspace --all-targets -- -D warnings`
- run `cargo test --workspace`

At the end, report:

- files created/changed
- commands run
- test results
- process discovery findings
- socket-to-PID findings
- required permissions
- known macOS limitations
- whether the Milestone 0 exit gate is satisfied

Do not continue to Milestone 1 unless Milestone 0 is verified.
