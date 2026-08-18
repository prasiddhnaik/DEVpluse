"use client";

/** Projects overview (task T4.3). */

import { useEffect, useState } from "react";

import { ProjectCard } from "@/components/ProjectCard";
import { SeriesChart } from "@/components/SeriesChart";
import { api } from "@/lib/api";
import { isDemoMode } from "@/lib/demo";
import { localPulseWorker, useDaemon, type ConnectionState } from "@/lib/daemon";
import { loadAvg } from "@/lib/format";
import type { HostSample, Status } from "@/lib/types";

const STATUS_POLL_MS = 2_000;

export function Overview() {
  const { projects, connection, status } = useDaemon();
  const [polled, setPolled] = useState<Status | null>(null);

  useEffect(() => {
    if (isDemoMode() || connection !== "connected") return;
    const controller = new AbortController();
    const load = () =>
      api
        .status(controller.signal)
        .then(setPolled)
        .catch(() => {
          /* snapshot status remains the fallback */
        });
    void load();
    const timer = setInterval(() => void load(), STATUS_POLL_MS);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [connection]);

  if (connection === "disconnected") {
    return <DaemonDown />;
  }

  const hostStatus = polled ?? status;
  const history = hostStatus?.host_history ?? [];

  if (projects.length === 0) {
    return (
      <div className="flex min-w-0 flex-col gap-6">
        <HostStrip history={history} latest={hostStatus?.host ?? null} />
        <EmptyState
          connection={connection}
          degraded={
            status?.collectors.process.degraded_fields?.cwd
              ? Object.entries(status.collectors.process.degraded_fields)
              : []
          }
        />
      </div>
    );
  }

  const sorted = [...projects].sort(
    (a, b) => b.running_service_count - a.running_service_count || a.name.localeCompare(b.name),
  );

  return (
    <div className="flex min-w-0 flex-col gap-6">
      <HostStrip history={history} latest={hostStatus?.host ?? null} />
      <div className="grid min-w-0 grid-cols-[repeat(auto-fit,minmax(min(100%,18rem),26rem))] gap-4">
        {sorted.map((project) => (
          <ProjectCard key={project.id} project={project} />
        ))}
      </div>
    </div>
  );
}

function HostStrip({
  history,
  latest,
}: {
  history: HostSample[];
  latest: HostSample | null | undefined;
}) {
  if (history.length < 2 && !latest) return null;

  return (
    <section className="flex min-w-0 flex-col gap-3">
      <h2 className="text-sm font-medium text-zinc-500">This machine</h2>
      <div className="grid min-w-0 gap-4 sm:grid-cols-2">
        <div className="min-w-0 overflow-hidden rounded-lg border border-line bg-surface-raised">
          <p className="px-3 pt-2 text-xs text-zinc-500">
            Load average
            {latest
              ? ` · 1m ${loadAvg(latest.load_avg_1)} · 5m ${loadAvg(latest.load_avg_5)} · 15m ${loadAvg(latest.load_avg_15)}`
              : ""}
          </p>
          <SeriesChart
            points={history.map((sample) => ({ at: sample.at, value: sample.load_avg_1 }))}
            format={loadAvg}
            strokeClass="stroke-sky-500"
            fillClass="fill-sky-500"
            label="load 1m"
            caption="sysinfo load average, 1-minute"
            empty="Waiting for host samples."
          />
        </div>
        <div className="min-w-0 overflow-hidden rounded-lg border border-line bg-surface-raised">
          <p className="px-3 pt-2 text-xs text-zinc-500">
            Process table
            {latest ? ` · ${latest.process_count} now` : ""}
          </p>
          <SeriesChart
            points={history.map((sample) => ({
              at: sample.at,
              value: sample.process_count,
            }))}
            format={(value) => String(Math.round(value))}
            strokeClass="stroke-amber-500"
            fillClass="fill-amber-500"
            label="processes"
            caption="processes.len() of the last snapshot"
            empty="Waiting for host samples."
          />
        </div>
      </div>
    </section>
  );
}

function emptyCopy(connection: ConnectionState): string {
  switch (connection) {
    case "connected":
      return "Runscape groups processes into projects by their working directory. Start a dev server inside a git repository and it will appear here within a second.";
    case "reconnecting":
      return `The ${localPulseWorker()} dropped; the dashboard is reconnecting and will refill this view.`;
    case "connecting":
      return `Connecting to ${localPulseWorker()}…`;
    case "disconnected":
      return `The ${localPulseWorker()} is not running.`;
    default: {
      const _exhaustive: never = connection;
      return _exhaustive;
    }
  }
}

function EmptyState({
  connection,
  degraded,
}: {
  connection: ConnectionState;
  degraded: [string, number][];
}) {
  return (
    <div className="rounded-xl border border-dashed border-line p-8 text-sm">
      <h2 className="text-base font-medium">No projects running</h2>
      <p className="pt-2 text-zinc-500">{emptyCopy(connection)}</p>
      {degraded.length > 0 && (
        <p className="pt-3 text-xs text-zinc-500">
          The process collector could not read{" "}
          {degraded.map(([field, count]) => `${field} (${count})`).join(", ")} —
          those processes belong to other users. Running Runscape as your own
          user shows your own work; running it as root widens the view.
        </p>
      )}
    </div>
  );
}

function DaemonDown() {
  return (
    <div className="rounded-xl border border-dashed border-rose-900 p-8 text-sm">
      <h2 className="text-base font-medium">The {localPulseWorker()} is not running</h2>
      <p className="pt-2 text-zinc-500">Start it and this page will reconnect:</p>
      <pre className="mt-3 w-fit rounded-md border border-line bg-zinc-800 px-3 py-2 font-mono text-xs text-ink">
        runscape serve
      </pre>
    </div>
  );
}
