/** Display helpers. Formatting only — nothing here decides anything. */

import type { RunscapeEvent, EventKind, Health, Severity } from "./types";

export function bytes(value: number): string {
  const units = [
    ["GB", 1024 ** 3],
    ["MB", 1024 ** 2],
    ["KB", 1024],
  ] as const;
  for (const [unit, size] of units) {
    if (value >= size) {
      const scaled = value / size;
      return `${scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1)} ${unit}`;
    }
  }
  return `${value} B`;
}

export function percent(value: number): string {
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)}%`;
}

/** Load averages are dimensionless; two decimals is what `uptime` prints. */
export function loadAvg(value: number): string {
  return value.toFixed(2);
}

/** Disk I/O on a ResourceSample is a per-tick delta, not a lifetime total. */
export function bytesPerTick(value: number): string {
  return `${bytes(value)} / tick`;
}

export function ago(iso: string, now = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";

  const seconds = Math.max(0, Math.round((now - then) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export function clock(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "—";
  return at.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export function offset(ms: number): string {
  const seconds = ms / 1000;
  const rendered = Math.abs(seconds) < 10 ? seconds.toFixed(1) : seconds.toFixed(0);
  return `${seconds > 0 ? "+" : ""}${rendered}s`;
}

/** Tailwind classes per health, used for dots, borders and text alike. */
export const healthStyle: Record<
  Health,
  { dot: string; svg: string; text: string; label: string }
> = {
  healthy: {
    dot: "bg-emerald-500",
    svg: "fill-emerald-500",
    text: "text-emerald-600 dark:text-emerald-400",
    label: "healthy",
  },
  degraded: {
    dot: "bg-amber-500",
    svg: "fill-amber-500",
    text: "text-amber-600 dark:text-amber-400",
    label: "degraded",
  },
  stopped: {
    dot: "bg-zinc-400",
    svg: "fill-zinc-400",
    text: "text-zinc-500 dark:text-zinc-400",
    label: "stopped",
  },
  unknown: {
    dot: "bg-zinc-300 dark:bg-zinc-600",
    svg: "fill-zinc-300 dark:fill-zinc-600",
    text: "text-zinc-500",
    label: "unknown",
  },
};

export const severityStyle: Record<Severity, string> = {
    info: "border-indigo-800 bg-indigo-950 text-indigo-200",
  warning:
    "border-amber-800 bg-amber-950 text-amber-200",
  critical:
    "border-rose-800 bg-rose-950 text-rose-200",
};

/** One line of English for an event. The daemon's `kind` is the source. */
export function describeEvent(event: RunscapeEvent, nameOf: (id: string) => string): string {
  const kind: EventKind = event.kind;
  switch (kind.type) {
    case "project_detected":
      return "project detected";
    case "service_started":
      return `${nameOf(kind.service_id)} started${kind.pid ? ` (pid ${kind.pid})` : ""}`;
    case "service_stopped":
      return `${nameOf(kind.service_id)} stopped${kind.pid ? ` (pid ${kind.pid})` : ""}`;
    case "service_restarted":
      return `${nameOf(kind.service_id)} restarted${
        kind.old_pid && kind.new_pid ? ` (pid ${kind.old_pid} → ${kind.new_pid})` : ""
      }`;
    case "port_opened":
      return `port ${kind.port} opened${
        kind.service_id ? ` by ${nameOf(kind.service_id)}` : ""
      }`;
    case "port_closed":
      return `port ${kind.port} closed${
        kind.service_id ? ` by ${nameOf(kind.service_id)}` : ""
      }`;
    case "connection_started":
      return `${nameOf(kind.source)} → ${nameOf(kind.target)}:${kind.target_port}`;
    case "connection_ended":
      return "connection ended";
    case "health_changed":
      return `${nameOf(kind.service_id)} went ${kind.from} → ${kind.to}`;
    case "resource_warning":
      return `${nameOf(kind.service_id)}: ${kind.detail}`;
    case "file_changed":
      return `file changed: ${shortenPath(kind.path)}`;
    default: {
      const _exhaustive: never = kind;
      return _exhaustive;
    }
  }
}

/** Event kinds worth colouring differently in a timeline. */
export function eventTone(event: RunscapeEvent): string {
  switch (event.kind.type) {
    case "service_started":
      return "bg-emerald-500";
    case "service_stopped":
      return "bg-zinc-400";
    case "service_restarted":
      return "bg-amber-500";
    case "health_changed":
      return event.kind.to === "healthy" ? "bg-emerald-500" : "bg-rose-500";
    case "file_changed":
      return "bg-violet-500";
    case "connection_started":
    case "connection_ended":
      return "bg-sky-500";
    case "project_detected":
    case "port_opened":
    case "port_closed":
    case "resource_warning":
      return "bg-zinc-300 dark:bg-zinc-600";
    default: {
      const _exhaustive: never = event.kind;
      return _exhaustive;
    }
  }
}

/** Keep the tail of a path: the end is the part a developer recognises. */
export function shortenPath(path: string, segments = 3): string {
  const parts = path.split("/").filter(Boolean);
  if (parts.length <= segments) return path;
  return `…/${parts.slice(-segments).join("/")}`;
}
