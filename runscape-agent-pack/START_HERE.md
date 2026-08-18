# Runscape — Agent Start Here

You are implementing **Runscape**, a local-first developer runtime discovery and debugging tool.

## Product goal

A developer should be able to run:

```bash
runscape
```

and immediately see the projects and services already running on their machine, including:

- processes
- ports
- CPU / memory
- local service-to-service connections
- Docker containers
- health state
- recent starts / stops / restarts
- a live topology graph
- a short event timeline

The central promise is:

> Show developers what their local code is doing without forcing them to configure it first.

## Important product boundary

Runscape is **not**:

- a Datadog clone
- a Docker Desktop clone
- a process orchestrator
- a packet sniffer
- an OpenTelemetry-only viewer
- an AI chat wrapper

The differentiator is:

1. zero-config local discovery
2. automatic project grouping
3. automatic service topology reconstruction
4. change/event correlation
5. local-first privacy

## Required reading order

Before writing implementation code, read:

1. `AGENTS.md`
2. `SPEC.md`
3. `ARCHITECTURE.md`
4. `TASKS.md`
5. `TEST_PLAN.md`
6. `DECISIONS.md`

Then start with **Milestone 0** in `TASKS.md`.

Do not skip the technical spikes. The hardest assumption in this project is whether reliable process/socket/project discovery works well enough on real developer machines.
