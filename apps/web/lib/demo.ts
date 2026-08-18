/**
 * Fixture snapshot for the public demo. Hosted dashboards must never talk to a
 * loopback observer — the product binds 127.0.0.1 only.
 *
 * Every field here is a sample of a daemon-owned shape. Nothing is invented at
 * render time beyond formatting (`AGENTS.md` rule 8).
 */

import type {
  Connection,
  Graph,
  HostSample,
  ProjectDetail,
  ProjectSummary,
  ResourceSample,
  RunscapeEvent,
  Service,
  ServiceDetail,
  Status,
} from "./types";

/** Build-time flag. Also treated as demo when the page is not on loopback. */
export function isDemoMode(): boolean {
  if (process.env.NEXT_PUBLIC_RUNSCAPE_DEMO === "1") return true;
  if (typeof window === "undefined") return false;
  const host = window.location.hostname;
  return host !== "127.0.0.1" && host !== "localhost" && host !== "[::1]";
}

const T0 = Date.parse("2026-08-18T18:00:00Z");
const SAMPLE_COUNT = 60;

function iso(offsetSec: number): string {
  return new Date(T0 + offsetSec * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");
}

function resourceHistory(): ResourceSample[] {
  const samples: ResourceSample[] = [];
  for (let i = 0; i < SAMPLE_COUNT; i += 1) {
    const wave = Math.sin(i / 8);
    samples.push({
      at: iso(i),
      cpu_percent: 4 + wave * 3 + (i % 7) * 0.2,
      memory_bytes: 38_000_000 + i * 120_000 + wave * 800_000,
      virtual_memory_bytes: 128_000_000 + i * 40_000,
      thread_count: 11 + (i % 4),
      disk_read_bytes: i % 5 === 0 ? 8_192 + i * 40 : 512,
      disk_write_bytes: i % 6 === 0 ? 4_096 : 128,
      connection_count: 1 + (i % 3 === 0 ? 1 : 0),
    });
  }
  return samples;
}

function hostHistory(): HostSample[] {
  const samples: HostSample[] = [];
  for (let i = 0; i < SAMPLE_COUNT; i += 1) {
    const wave = Math.sin(i / 11);
    samples.push({
      at: iso(i),
      load_avg_1: 1.1 + wave * 0.35,
      load_avg_5: 1.05 + wave * 0.2,
      load_avg_15: 0.98 + wave * 0.1,
      process_count: 390 + Math.round(wave * 12) + (i % 5),
    });
  }
  return samples;
}

const HISTORY = resourceHistory();
const HOST_HISTORY = hostHistory();
const LAST = iso(SAMPLE_COUNT - 1);

const evidence = {
  evidence_type: "observed_socket" as const,
  confidence: 1,
  first_seen: iso(0),
  last_seen: LAST,
  detail: null,
};

export const DEMO_CONNECTION: Connection = {
  id: "con_demo_web_api",
  source: "svc_demo_web",
  target: "svc_demo_api",
  target_port: 41011,
  evidence,
};

export const DEMO_SERVICES: Service[] = [
  {
    id: "svc_demo_web",
    project_id: "prj_demo",
    name: "web",
    kind: { kind: "host_process" },
    runtime: "node",
    fingerprint: "host|prj_demo|node|web",
    health: "healthy",
    restart_count: 0,
    first_seen: iso(0),
    last_seen: LAST,
    instances: [
      {
        pid: 76466,
        parent_pid: 71309,
        executable: "/opt/homebrew/bin/node",
        command: ["node", "server.js", "--api-key", "<redacted>"],
        cwd: "/private/tmp/runscape-demo/web",
        started_at: iso(0),
        cpu_percent: HISTORY[HISTORY.length - 1]?.cpu_percent ?? 4,
        memory_bytes: HISTORY[HISTORY.length - 1]?.memory_bytes ?? 38_000_000,
      },
    ],
    endpoints: [{ address: "127.0.0.1", port: 41010, protocol: "tcp", pid: 76466 }],
    resource_history: HISTORY,
  },
  {
    id: "svc_demo_api",
    project_id: "prj_demo",
    name: "api",
    kind: { kind: "host_process" },
    runtime: "node",
    fingerprint: "host|prj_demo|node|api",
    health: "healthy",
    restart_count: 1,
    first_seen: iso(0),
    last_seen: LAST,
    instances: [
      {
        pid: 76490,
        parent_pid: 71309,
        executable: "/opt/homebrew/bin/node",
        command: ["node", "api.js"],
        cwd: "/private/tmp/runscape-demo/api",
        started_at: iso(12),
        cpu_percent: 1.2,
        memory_bytes: 22_000_000,
      },
    ],
    endpoints: [{ address: "127.0.0.1", port: 41011, protocol: "tcp", pid: 76490 }],
    resource_history: HISTORY.map((sample) => ({
      ...sample,
      cpu_percent: sample.cpu_percent * 0.4,
      memory_bytes: sample.memory_bytes * 0.6,
      thread_count: 6,
      connection_count: 1,
    })),
  },
];

export const DEMO_PROJECT: ProjectSummary = {
  id: "prj_demo",
  name: "runscape-demo",
  root: "/private/tmp/runscape-demo",
  kind: "git_repository",
  confidence: 0.95,
  first_seen: iso(0),
  last_seen: LAST,
  service_count: 2,
  running_service_count: 2,
  health: "healthy",
  memory_bytes: 60_000_000,
  cpu_percent: 5.2,
  recent_warning: null,
};

export const DEMO_EVENTS: RunscapeEvent[] = [
  {
    id: "evt_demo_1",
    at: iso(0),
    project_id: "prj_demo",
    kind: { type: "project_detected", project_id: "prj_demo" },
  },
  {
    id: "evt_demo_2",
    at: iso(1),
    project_id: "prj_demo",
    kind: { type: "service_started", service_id: "svc_demo_web", pid: 76466 },
  },
  {
    id: "evt_demo_3",
    at: iso(12),
    project_id: "prj_demo",
    kind: { type: "service_started", service_id: "svc_demo_api", pid: 76490 },
  },
  {
    id: "evt_demo_4",
    at: iso(20),
    project_id: "prj_demo",
    kind: {
      type: "connection_started",
      connection_id: DEMO_CONNECTION.id,
      source: DEMO_CONNECTION.source,
      target: DEMO_CONNECTION.target,
      target_port: DEMO_CONNECTION.target_port,
    },
  },
];

export const DEMO_STATUS: Status = {
  version: "0.1.0",
  started_at: iso(0),
  uptime_ms: SAMPLE_COUNT * 1000,
  platform: {
    os: "macos",
    process_list: "full",
    process_cwd: "same_user_only",
    process_exe: "same_user_only",
    process_command: "same_user_only",
    socket_list: "same_user_only",
    socket_owner_pid: "same_user_only",
    root_widens_view: true,
    notes: ["Demo fixture — not collected from this machine."],
  },
  docker: { available: false, reason: "Demo has no Docker collector" },
  counts: { projects: 1, services: 2, connections: 1, events: DEMO_EVENTS.length },
  collectors: {
    process: { last_duration_ms: 6, last_run: LAST },
    socket: { last_duration_ms: 1, last_run: LAST, sockets_without_owner: 0 },
  },
  host: HOST_HISTORY[HOST_HISTORY.length - 1],
  host_history: HOST_HISTORY,
};

export const DEMO_PROJECT_DETAIL: ProjectDetail = {
  project: DEMO_PROJECT,
  resource_history: HISTORY.map((sample, index) => {
    const other = DEMO_SERVICES[1]?.resource_history?.[index];
    return {
      ...sample,
      cpu_percent: sample.cpu_percent + (other?.cpu_percent ?? 0),
      memory_bytes: sample.memory_bytes + (other?.memory_bytes ?? 0),
      virtual_memory_bytes:
        sample.virtual_memory_bytes + (other?.virtual_memory_bytes ?? 0),
      thread_count: sample.thread_count + (other?.thread_count ?? 0),
      disk_read_bytes: sample.disk_read_bytes + (other?.disk_read_bytes ?? 0),
      disk_write_bytes: sample.disk_write_bytes + (other?.disk_write_bytes ?? 0),
      connection_count: sample.connection_count + (other?.connection_count ?? 0),
    };
  }),
  services: DEMO_SERVICES,
  connections: [DEMO_CONNECTION],
  warnings: [],
  recent_events: [...DEMO_EVENTS].reverse(),
};

export const DEMO_GRAPH: Graph = {
  project_id: "prj_demo",
  nodes: DEMO_SERVICES.map((service) => ({
    id: service.id,
    name: service.name,
    runtime: service.runtime,
    health: service.health,
    port: service.endpoints[0]?.port ?? null,
    cpu_percent: service.instances.reduce((sum, i) => sum + i.cpu_percent, 0),
    memory_bytes: service.instances.reduce((sum, i) => sum + i.memory_bytes, 0),
    kind: service.kind.kind,
  })),
  edges: [DEMO_CONNECTION],
};

export function demoServiceDetail(id: string): ServiceDetail | null {
  const service = DEMO_SERVICES.find((item) => item.id === id);
  if (!service) return null;
  const outbound = DEMO_CONNECTION.source === id ? [DEMO_CONNECTION] : [];
  const inbound = DEMO_CONNECTION.target === id ? [DEMO_CONNECTION] : [];
  return {
    ...service,
    connections: { outbound, inbound },
    recent_events: DEMO_EVENTS.filter((event) => {
      const kind = event.kind;
      switch (kind.type) {
        case "service_started":
        case "service_stopped":
        case "service_restarted":
        case "health_changed":
        case "resource_warning":
          return kind.service_id === id;
        case "port_opened":
        case "port_closed":
          return kind.service_id === id;
        case "connection_started":
          return kind.source === id || kind.target === id;
        case "connection_ended":
          return kind.connection_id === DEMO_CONNECTION.id;
        case "project_detected":
        case "file_changed":
          return false;
        default: {
          const _exhaustive: never = kind;
          return _exhaustive;
        }
      }
    }).reverse(),
  };
}
