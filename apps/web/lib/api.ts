/**
 * HTTP client for the local daemon.
 *
 * The WebSocket carries the live view; these calls are the cold start and the
 * detail views. Everything is a `GET`, because the daemon exposes nothing else
 * (`DECISIONS.md` D004).
 */

import type {
  EventContext,
  RunscapeEvent,
  Graph,
  ProjectDetail,
  ProjectSummary,
  ServiceDetail,
  Status,
  Warning,
} from "./types";
import {
  DEMO_EVENTS,
  DEMO_GRAPH,
  DEMO_PROJECT,
  DEMO_PROJECT_DETAIL,
  DEMO_STATUS,
  demoServiceDetail,
  isDemoMode,
} from "./demo";

/** Where the daemon listens during `next dev` (dashboard on :3000). */
export const DAEMON_HTTP =
  process.env.NEXT_PUBLIC_RUNSCAPE_HTTP ?? "http://127.0.0.1:2013";

export const DAEMON_WS =
  process.env.NEXT_PUBLIC_RUNSCAPE_WS ?? "ws://127.0.0.1:2013/ws/v1";

/** True when this page is served by `runscape serve`, not `next dev`. */
function isEmbeddedUi(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  const { hostname, port } = window.location;
  const loopback =
    hostname === "127.0.0.1" || hostname === "localhost" || hostname === "[::1]";
  return loopback && port !== "3000";
}

export function daemonHttp(): string {
  if (isEmbeddedUi()) {
    return window.location.origin;
  }
  return DAEMON_HTTP;
}

export function daemonWs(): string {
  if (isEmbeddedUi()) {
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}/ws/v1`;
  }
  return DAEMON_WS;
}

/** An error the daemon reported in its own error shape. */
export class ApiError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  if (isDemoMode()) {
    return Promise.resolve(demoGet(path) as T);
  }

  const response = await fetch(`${daemonHttp()}${path}`, {
    signal,
    headers: { accept: "application/json" },
    cache: "no-store",
  });

  if (!response.ok) {
    const body = await response.json().catch(() => null);
    const error = (body as { error?: { code: string; message: string } } | null)
      ?.error;
    throw new ApiError(
      error?.code ?? "unavailable",
      error?.message ?? `${response.status} from ${path}`,
      response.status,
    );
  }

  return (await response.json()) as T;
}

function demoGet(path: string): unknown {
  const [pathname] = path.split("?");
  const url = pathname ?? path;
  if (url === "/api/v1/status") return DEMO_STATUS;
  if (url === "/api/v1/projects") return [DEMO_PROJECT];
  if (url === "/api/v1/projects/prj_demo") return DEMO_PROJECT_DETAIL;
  if (url.startsWith("/api/v1/projects/")) {
    throw new ApiError("not_found", "unknown project", 404);
  }
  if (url.startsWith("/api/v1/services/")) {
    const id = decodeURIComponent(url.slice("/api/v1/services/".length));
    const detail = demoServiceDetail(id);
    if (!detail) throw new ApiError("not_found", "unknown service", 404);
    return detail;
  }
  if (url.startsWith("/api/v1/graph/")) return DEMO_GRAPH;
  if (url.startsWith("/api/v1/events/") && url.endsWith("/context")) {
    const event = DEMO_EVENTS[0];
    if (!event) {
      throw new ApiError("unavailable", "demo fixture has no events", 500);
    }
    return {
      event,
      window_ms: 5_000,
      before: [],
      after: [],
    } satisfies EventContext;
  }
  if (url.startsWith("/api/v1/events")) return [...DEMO_EVENTS].reverse();
  if (url.startsWith("/api/v1/warnings")) return [];
  throw new ApiError("not_found", `no demo fixture for ${url}`, 404);
}

export const api = {
  status: (signal?: AbortSignal) => get<Status>("/api/v1/status", signal),

  projects: (signal?: AbortSignal) =>
    get<ProjectSummary[]>("/api/v1/projects", signal),

  project: (id: string, signal?: AbortSignal) =>
    get<ProjectDetail>(`/api/v1/projects/${encodeURIComponent(id)}`, signal),

  service: (id: string, signal?: AbortSignal) =>
    get<ServiceDetail>(`/api/v1/services/${encodeURIComponent(id)}`, signal),

  graph: (projectId: string, signal?: AbortSignal) =>
    get<Graph>(`/api/v1/graph/${encodeURIComponent(projectId)}`, signal),

  events: (
    params: {
      projectId?: string;
      serviceId?: string;
      limit?: number;
      since?: string;
    } = {},
    signal?: AbortSignal,
  ) => {
    const query = new URLSearchParams();
    if (params.projectId) query.set("project_id", params.projectId);
    if (params.serviceId) query.set("service_id", params.serviceId);
    if (params.limit) query.set("limit", String(params.limit));
    if (params.since) query.set("since", params.since);
    const suffix = query.size > 0 ? `?${query}` : "";
    return get<RunscapeEvent[]>(`/api/v1/events${suffix}`, signal);
  },

  eventContext: (id: string, windowMs?: number, signal?: AbortSignal) => {
    const suffix = windowMs ? `?window_ms=${windowMs}` : "";
    return get<EventContext>(
      `/api/v1/events/${encodeURIComponent(id)}/context${suffix}`,
      signal,
    );
  },

  warnings: (projectId?: string, signal?: AbortSignal) => {
    const suffix = projectId
      ? `?project_id=${encodeURIComponent(projectId)}`
      : "";
    return get<Warning[]>(`/api/v1/warnings${suffix}`, signal);
  },
};
