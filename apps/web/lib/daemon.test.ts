/**
 * The frame reducer is the only place the dashboard holds state, so it is the
 * only place it can be wrong about what the daemon said. These tests pin the
 * frame handling from `docs/api-contract.md`.
 */

import { describe, expect, test } from "bun:test";

import { reduce, type DaemonView } from "./daemon";
import type { Connection, DevPulseEvent, ServerFrame, Service, Warning } from "./types";

const base: DaemonView = {
  connection: "connecting",
  reconnects: 0,
  status: null,
  projects: [],
  services: [],
  connections: [],
  warnings: [],
  events: [],
  lastFrameAt: null,
  resnapshot: () => {},
};

function service(id: string, name: string, health: Service["health"] = "healthy"): Service {
  return {
    id,
    project_id: "prj_1",
    name,
    kind: { kind: "host_process" },
    runtime: "node",
    fingerprint: `host|${name}`,
    health,
    restart_count: 0,
    first_seen: "2026-08-18T10:00:00Z",
    last_seen: "2026-08-18T10:00:10Z",
    instances: [],
    endpoints: [],
  };
}

function connection(id: string, source: string, target: string): Connection {
  return {
    id,
    source,
    target,
    target_port: 5432,
    evidence: {
      evidence_type: "observed_socket",
      confidence: 1,
      first_seen: "2026-08-18T10:00:00Z",
      last_seen: "2026-08-18T10:00:10Z",
      detail: null,
    },
  };
}

function warning(id: string): Warning {
  return {
    id,
    rule: "restart_loop",
    severity: "critical",
    project_id: "prj_1",
    service_id: "svc_a",
    message: "web restarted 3 times in the last 60s",
    first_seen: "2026-08-18T10:00:00Z",
    last_seen: "2026-08-18T10:00:10Z",
    related_events: [],
  };
}

function event(id: string, at: string): DevPulseEvent {
  return {
    id,
    at,
    project_id: "prj_1",
    kind: { type: "service_started", service_id: "svc_a", pid: 100 },
  };
}

describe("services_changed", () => {
  test("replaces a service that changed", () => {
    const start: DaemonView = { ...base, services: [service("svc_a", "web")] };
    const frame: ServerFrame = {
      type: "services_changed",
      at: "2026-08-18T10:00:11Z",
      services: [service("svc_a", "web", "degraded")],
      removed: [],
    };

    const next = reduce(start, frame);
    expect(next.services).toHaveLength(1);
    expect(next.services[0]?.health).toBe("degraded");
  });

  test("adds a service it has not seen before", () => {
    const next = reduce(base, {
      type: "services_changed",
      at: "2026-08-18T10:00:11Z",
      services: [service("svc_b", "api")],
      removed: [],
    });
    expect(next.services.map((s) => s.name)).toEqual(["api"]);
  });

  test("drops removed services", () => {
    const start: DaemonView = {
      ...base,
      services: [service("svc_a", "web"), service("svc_b", "api")],
    };
    const next = reduce(start, {
      type: "services_changed",
      at: "2026-08-18T10:00:11Z",
      services: [],
      removed: ["svc_a"],
    });
    expect(next.services.map((s) => s.id)).toEqual(["svc_b"]);
  });
});

describe("topology_changed", () => {
  test("adds an edge without duplicating it", () => {
    const start: DaemonView = { ...base, connections: [connection("con_1", "a", "b")] };
    const next = reduce(start, {
      type: "topology_changed",
      at: "2026-08-18T10:00:11Z",
      project_id: "prj_1",
      added: [connection("con_1", "a", "b"), connection("con_2", "b", "c")],
      removed: [],
    });

    expect(next.connections.map((c) => c.id).sort()).toEqual(["con_1", "con_2"]);
  });

  test("removes an edge the daemon dropped", () => {
    const start: DaemonView = { ...base, connections: [connection("con_1", "a", "b")] };
    const next = reduce(start, {
      type: "topology_changed",
      at: "2026-08-18T10:00:11Z",
      project_id: "prj_1",
      added: [],
      removed: ["con_1"],
    });
    expect(next.connections).toHaveLength(0);
  });
});

describe("warnings_changed", () => {
  test("adds and clears warnings", () => {
    const added = reduce(base, {
      type: "warnings_changed",
      at: "2026-08-18T10:00:11Z",
      warnings: [warning("warn_1")],
      removed: [],
    });
    expect(added.warnings).toHaveLength(1);

    const cleared = reduce(added, {
      type: "warnings_changed",
      at: "2026-08-18T10:00:12Z",
      warnings: [],
      removed: ["warn_1"],
    });
    expect(cleared.warnings).toHaveLength(0);
  });
});

describe("events", () => {
  test("keeps the newest event first", () => {
    const next = reduce(base, {
      type: "events",
      at: "2026-08-18T10:00:11Z",
      events: [event("evt_1", "2026-08-18T10:00:01Z"), event("evt_2", "2026-08-18T10:00:02Z")],
    });

    expect(next.events.map((e) => e.id)).toEqual(["evt_2", "evt_1"]);
  });

  test("is bounded, so a long session cannot grow without limit", () => {
    let view = base;
    for (let i = 0; i < 700; i += 1) {
      view = reduce(view, {
        type: "events",
        at: "2026-08-18T10:00:11Z",
        events: [event(`evt_${i}`, "2026-08-18T10:00:01Z")],
      });
    }
    expect(view.events.length).toBeLessThanOrEqual(500);
    expect(view.events[0]?.id).toBe("evt_699");
  });
});

describe("snapshot", () => {
  test("replaces the whole view, which is why a reconnect asks for one", () => {
    const stale: DaemonView = {
      ...base,
      services: [service("svc_gone", "old")],
      connections: [connection("con_gone", "a", "b")],
      warnings: [warning("warn_gone")],
      connection: "reconnecting",
    };

    const next = reduce(stale, {
      type: "snapshot",
      at: "2026-08-18T10:00:11Z",
      status: {
        version: "0.1.0",
        started_at: "2026-08-18T09:00:00Z",
        uptime_ms: 1000,
        platform: {
          os: "macos",
          process_list: "full",
          process_cwd: "same_user_only",
          process_exe: "same_user_only",
          process_command: "same_user_only",
          socket_list: "same_user_only",
          socket_owner_pid: "same_user_only",
          root_widens_view: true,
          notes: [],
        },
        docker: { available: false },
        counts: { projects: 0, services: 1, connections: 0, events: 0 },
        collectors: {
          process: { last_duration_ms: 4, last_run: null },
          socket: { last_duration_ms: 1, last_run: null },
        },
      },
      projects: [],
      services: [service("svc_a", "web")],
      connections: [],
      warnings: [],
    });

    expect(next.connection).toBe("connected");
    expect(next.services.map((s) => s.id)).toEqual(["svc_a"]);
    expect(next.connections).toHaveLength(0);
    expect(next.warnings).toHaveLength(0);
  });
});
