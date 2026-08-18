/**
 * HTTP client for the local daemon.
 *
 * The WebSocket carries the live view; these calls are the cold start and the
 * detail views. Everything is a `GET`, because the daemon exposes nothing else
 * (`DECISIONS.md` D004).
 */

import type {
  EventContext,
  DevPulseEvent,
  Graph,
  ProjectDetail,
  ProjectSummary,
  ServiceDetail,
  Status,
  Warning,
} from "./types";

/** Where the daemon listens. Override for a daemon on another port. */
export const DAEMON_HTTP =
  process.env.NEXT_PUBLIC_DEVPULSE_HTTP ?? "http://127.0.0.1:7778";

export const DAEMON_WS =
  process.env.NEXT_PUBLIC_DEVPULSE_WS ?? "ws://127.0.0.1:7778/ws/v1";

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
  const response = await fetch(`${DAEMON_HTTP}${path}`, {
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
    return get<DevPulseEvent[]>(`/api/v1/events${suffix}`, signal);
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
