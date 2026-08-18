"use client";

/** Projects overview (task T4.3). */

import Link from "next/link";

import { HealthDot } from "@/components/HealthDot";
import { localPulseWorker, useDaemon, type ConnectionState } from "@/lib/daemon";
import { ago, bytes, healthStyle, percent, severityStyle, shortenPath } from "@/lib/format";
import type { ProjectSummary } from "@/lib/types";

export default function ProjectsPage() {
  const { projects, connection, status } = useDaemon();

  if (connection === "disconnected") {
    return <DaemonDown />;
  }

  if (projects.length === 0) {
    return (
      <EmptyState
        connection={connection}
        degraded={
          status?.collectors.process.degraded_fields?.cwd
            ? Object.entries(status.collectors.process.degraded_fields)
            : []
        }
      />
    );
  }

  const sorted = [...projects].sort(
    (a, b) => b.running_service_count - a.running_service_count || a.name.localeCompare(b.name),
  );

  return (
    <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,18rem),1fr))] gap-4">
      {sorted.map((project) => (
        <ProjectCard key={project.id} project={project} />
      ))}
    </div>
  );
}

function ProjectCard({ project }: { project: ProjectSummary }) {
  const style = healthStyle[project.health];

  return (
    <Link
      href={`/projects/${project.id}`}
      className="flex flex-col gap-3 rounded-xl border border-line bg-surface-raised p-4 transition hover:border-white/15"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="truncate text-base font-medium">{project.name}</h2>
          <p className="truncate text-xs text-zinc-500" title={project.root}>
            {shortenPath(project.root, 2)}
          </p>
        </div>
        <span className={`flex items-center gap-1.5 text-xs ${style.text}`}>
          <HealthDot health={project.health} />
          {style.label}
        </span>
      </div>

      <dl className="grid grid-cols-3 gap-2 text-sm">
        <Metric
          label="services"
          value={`${project.running_service_count}/${project.service_count}`}
        />
        <Metric label="memory" value={bytes(project.memory_bytes)} />
        <Metric label="cpu" value={percent(project.cpu_percent)} />
      </dl>

      {project.recent_warning ? (
        <p
          className={`rounded-md border px-2 py-1 text-xs ${severityStyle[project.recent_warning.severity]}`}
        >
          {project.recent_warning.message}
        </p>
      ) : (
        <p className="text-xs text-zinc-400">
          {project.kind.replace(/_/g, " ")} · seen {ago(project.last_seen)}
        </p>
      )}
    </Link>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-zinc-500">{label}</dt>
      <dd className="font-medium tabular-nums">{value}</dd>
    </div>
  );
}

function emptyCopy(connection: ConnectionState): string {
  switch (connection) {
    case "connected":
      return "DevPulse groups processes into projects by their working directory. Start a dev server inside a git repository and it will appear here within a second.";
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
          those processes belong to other users. Running DevPulse as your own
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
        devpulse serve
      </pre>
    </div>
  );
}
