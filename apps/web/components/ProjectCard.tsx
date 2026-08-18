"use client";

import { HealthDot } from "@/components/HealthDot";
import { NavLink } from "@/components/NavLink";
import { ago, bytes, healthStyle, percent, severityStyle, shortenPath } from "@/lib/format";
import type { ProjectSummary, RankedService } from "@/lib/types";

const CHIP_LIMIT = 8;

export function projectStateLabel(project: ProjectSummary): string {
  if (project.running_service_count === 0) return "not running";
  return healthStyle[project.health].label;
}

export function isProjectIdle(project: ProjectSummary): boolean {
  return project.running_service_count === 0;
}

export function ProjectCard({ project }: { project: ProjectSummary }) {
  const style = healthStyle[project.health];
  const idle = isProjectIdle(project);

  return (
    <NavLink
      href={`/projects/${project.id}`}
      className="flex min-w-0 max-w-md flex-col gap-3 rounded-xl border border-line bg-surface-raised p-4 transition hover:border-white/15"
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
          {projectStateLabel(project)}
        </span>
      </div>

      {idle ? <IdleBody project={project} /> : <RunningBody project={project} />}
    </NavLink>
  );
}

function IdleBody({ project }: { project: ProjectSummary }) {
  return (
    <>
      {project.recent_warning ? (
        <p
          className={`rounded-md border px-2 py-1 text-xs ${severityStyle[project.recent_warning.severity]}`}
        >
          {project.recent_warning.message}
        </p>
      ) : null}
      <p className="text-xs text-zinc-500">
        Last seen with {project.service_count}{" "}
        {project.service_count === 1 ? "service" : "services"} ·{" "}
        {project.kind.replace(/_/g, " ")}
        {project.last_seen ? ` · ${ago(project.last_seen)}` : ""}
      </p>
    </>
  );
}

function RunningBody({ project }: { project: ProjectSummary }) {
  const byCpu = project.ranked_by_cpu ?? [];
  const byMemory = project.ranked_by_memory ?? [];

  return (
    <>
      <dl className="grid grid-cols-3 gap-2 text-sm">
        <Metric
          label="services"
          value={`${project.running_service_count}/${project.service_count}`}
        />
        <Metric label="memory" value={bytes(project.memory_bytes)} />
        <Metric label="cpu" value={percent(project.cpu_percent)} />
      </dl>

      {byCpu.length > 0 ? (
        <ul className="flex flex-wrap gap-1.5">
          {byCpu.slice(0, CHIP_LIMIT).map((service, index) => (
            <li key={service.id}>
              <ServiceChip service={service} hot={index === 0} />
            </li>
          ))}
        </ul>
      ) : null}

      <MemoryShare
        ranked={byMemory}
        totalBytes={project.memory_bytes}
        dominantId={project.dominant_memory?.id}
      />

      {project.recent_warning ? (
        <p
          className={`rounded-md border px-2 py-1 text-xs ${severityStyle[project.recent_warning.severity]}`}
        >
          {project.recent_warning.message}
        </p>
      ) : (
        <p className="text-xs text-zinc-500">
          {project.kind.replace(/_/g, " ")} · seen {ago(project.last_seen)}
        </p>
      )}
    </>
  );
}

function ServiceChip({ service, hot }: { service: RankedService; hot: boolean }) {
  return (
    <span
      className={`inline-flex max-w-full truncate rounded-full border px-2 py-0.5 text-[11px] ${
        hot
          ? "border-indigo-700 bg-indigo-950 text-indigo-100"
          : "border-line bg-zinc-950 text-zinc-300"
      }`}
      title={`${service.name} · ${percent(service.cpu_percent)} · ${bytes(service.memory_bytes)}`}
    >
      {service.name}
    </span>
  );
}

function MemoryShare({
  ranked,
  totalBytes,
  dominantId,
}: {
  ranked: RankedService[];
  totalBytes: number;
  dominantId?: string;
}) {
  const segments = ranked.filter(
    (service) => service.resources_measured && service.memory_bytes > 0,
  );
  const top = segments[0];
  if (!top || totalBytes === 0) {
    return null;
  }
  const measuredTotal = segments.reduce((sum, service) => sum + service.memory_bytes, 0);

  return (
    <div>
      <div className="flex items-baseline justify-between gap-2 text-[11px] text-zinc-500">
        <span className="truncate">
          {top.name} {bytes(top.memory_bytes)}
        </span>
        <span className="shrink-0 tabular-nums">{bytes(totalBytes)} RSS</span>
      </div>
      <div
        className="mt-1 flex h-1.5 overflow-hidden rounded-full bg-zinc-800"
        role="img"
        aria-label={`memory share, ${top.name} ${bytes(top.memory_bytes)} of ${bytes(totalBytes)}`}
      >
        {segments.map((service, index) => (
          <span
            key={service.id}
            className={segmentTone(index, service.id, dominantId)}
            style={{
              width: `${Math.max((service.memory_bytes / measuredTotal) * 100, 0)}%`,
            }}
            title={`${service.name} ${bytes(service.memory_bytes)}`}
          />
        ))}
      </div>
    </div>
  );
}

function segmentTone(index: number, id: string, dominantId?: string): string {
  if (dominantId && id === dominantId) return "bg-amber-500";
  if (index === 0) return "bg-indigo-500";
  return index % 2 === 0 ? "bg-zinc-500" : "bg-zinc-600";
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-zinc-500">{label}</dt>
      <dd className="font-medium tabular-nums">{value}</dd>
    </div>
  );
}
