"use client";

/**
 * The live connection to the daemon (task T4.2).
 *
 * One WebSocket for the whole app: it delivers a snapshot on connect and
 * incremental frames afterwards. On reconnect the client asks for a fresh
 * snapshot rather than trusting that it missed nothing (`ARCHITECTURE.md`).
 *
 * This holds a *copy* of the daemon's view for rendering. It never derives new
 * runtime facts from it — no health guessing, no topology inference. If the
 * dashboard needs to know something, the daemon says it (`AGENTS.md` rule 8).
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { DAEMON_WS } from "./api";
import type {
  Connection,
  DevPulseEvent,
  ProjectSummary,
  ServerFrame,
  Service,
  Status,
  Warning,
} from "./types";

export type ConnectionState = "connecting" | "connected" | "reconnecting" | "disconnected";

/** Events kept for the timeline. The daemon holds far more; ask it for those. */
const EVENT_BUFFER = 500;

/** Reconnect backoff, in milliseconds. The last value repeats. */
const BACKOFF = [500, 1_000, 2_000, 5_000, 10_000] as const;

export interface DaemonView {
  connection: ConnectionState;
  /** How many times the socket has come back. Shown so a flapping daemon is visible. */
  reconnects: number;
  status: Status | null;
  projects: ProjectSummary[];
  services: Service[];
  connections: Connection[];
  warnings: Warning[];
  /** Newest first. */
  events: DevPulseEvent[];
  /** When the last frame arrived. */
  lastFrameAt: string | null;
  /** Ask the daemon for a fresh snapshot. */
  resnapshot: () => void;
}

const empty: DaemonView = {
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

const DaemonContext = createContext<DaemonView>(empty);

export function useDaemon(): DaemonView {
  return useContext(DaemonContext);
}

/** The services of one project, in a stable order. */
export function useProjectServices(projectId: string | null): Service[] {
  const { services } = useDaemon();
  return useMemo(
    () =>
      services
        .filter((service) => service.project_id === projectId)
        .sort((a, b) => a.name.localeCompare(b.name)),
    [services, projectId],
  );
}

export function useProject(projectId: string): ProjectSummary | null {
  const { projects } = useDaemon();
  return projects.find((project) => project.id === projectId) ?? null;
}

export function DaemonProvider({ children }: { children: React.ReactNode }) {
  const [view, setView] = useState<DaemonView>(empty);
  const socket = useRef<WebSocket | null>(null);

  const resnapshot = useCallback(() => {
    socket.current?.send(JSON.stringify({ type: "resnapshot" }));
  }, []);

  useEffect(() => {
    let closed = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const apply = (frame: ServerFrame) => {
      setView((current) => reduce(current, frame));
    };

    const connect = () => {
      if (closed) return;

      const ws = new WebSocket(DAEMON_WS);
      socket.current = ws;

      ws.onopen = () => {
        attempt = 0;
        setView((current) => ({ ...current, connection: "connected" }));
      };

      ws.onmessage = (message) => {
        try {
          apply(JSON.parse(message.data as string) as ServerFrame);
        } catch {
          // A frame this build does not understand is ignored rather than
          // allowed to break the view.
        }
      };

      ws.onclose = () => {
        if (closed) return;
        // A daemon that went away is "reconnecting" while there is any hope of
        // it coming back, and honest about it after that.
        const delay = BACKOFF[Math.min(attempt, BACKOFF.length - 1)] ?? 10_000;
        attempt += 1;
        setView((current) => ({
          ...current,
          connection: attempt > BACKOFF.length ? "disconnected" : "reconnecting",
          reconnects: current.reconnects + (attempt === 1 ? 1 : 0),
        }));
        timer = setTimeout(connect, delay);
      };

      ws.onerror = () => ws.close();
    };

    connect();

    return () => {
      closed = true;
      if (timer) clearTimeout(timer);
      socket.current?.close();
      socket.current = null;
    };
  }, []);

  const value = useMemo<DaemonView>(
    () => ({ ...view, resnapshot }),
    [view, resnapshot],
  );

  return (
    <DaemonContext.Provider value={value}>{children}</DaemonContext.Provider>
  );
}

/** Fold one frame into the view. Pure, so it is testable and predictable. */
export function reduce(current: DaemonView, frame: ServerFrame): DaemonView {
  switch (frame.type) {
    case "snapshot":
      return {
        ...current,
        connection: "connected",
        status: frame.status,
        projects: frame.projects,
        services: frame.services,
        connections: frame.connections,
        warnings: frame.warnings,
        lastFrameAt: frame.at,
      };

    case "services_changed": {
      const changed = new Map(frame.services.map((s) => [s.id, s]));
      const removed = new Set(frame.removed);
      const kept = current.services
        .filter((service) => !removed.has(service.id))
        .map((service) => changed.get(service.id) ?? service);
      const added = frame.services.filter(
        (service) => !current.services.some((s) => s.id === service.id),
      );
      return { ...current, services: [...kept, ...added], lastFrameAt: frame.at };
    }

    case "topology_changed": {
      const removed = new Set(frame.removed);
      const kept = current.connections.filter(
        (connection) =>
          !removed.has(connection.id) &&
          !frame.added.some((added) => added.id === connection.id),
      );
      return {
        ...current,
        connections: [...kept, ...frame.added],
        lastFrameAt: frame.at,
      };
    }

    case "warnings_changed": {
      const removed = new Set(frame.removed);
      const kept = current.warnings.filter(
        (warning) =>
          !removed.has(warning.id) &&
          !frame.warnings.some((added) => added.id === warning.id),
      );
      return {
        ...current,
        warnings: [...frame.warnings, ...kept],
        lastFrameAt: frame.at,
      };
    }

    case "events":
      return {
        ...current,
        events: [...[...frame.events].reverse(), ...current.events].slice(
          0,
          EVENT_BUFFER,
        ),
        lastFrameAt: frame.at,
      };
    default: {
      const _exhaustive: never = frame;
      return _exhaustive;
    }
  }
}
