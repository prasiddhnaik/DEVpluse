"use client";

/**
 * Service inspector (task T4.5): processes, cwd, redacted command, ports,
 * resources, connections and recent events.
 *
 * The command line shown here was redacted by the daemon at capture time — the
 * dashboard has never seen the raw argv, and cannot un-redact it
 * (`AGENTS.md` rule 6).
 */

import Link from "next/link";
import { use, useEffect, useState } from "react";

import { HealthDot } from "@/components/HealthDot";
import { Timeline } from "@/components/Timeline";
import { api } from "@/lib/api";
import { useDaemon } from "@/lib/daemon";
import { ago, bytes, clock, healthStyle, percent } from "@/lib/format";
import type { Connection, ResourceSample, ServiceDetail } from "@/lib/types";

/** How often the inspector re-reads the detail endpoint while it is open. */
const REFRESH_MS = 2_000;

export default function ServicePage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  const { services } = useDaemon();
  const [detail, setDetail] = useState<ServiceDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();

    const load = () =>
      api
        .service(id, controller.signal)
        .then((next) => {
          setDetail(next);
          setError(null);
        })
        .catch((cause: unknown) => {
          if (controller.signal.aborted) return;
          setError(cause instanceof Error ? cause.message : "unavailable");
        });

    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);

    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [id]);

  if (error && !detail) {
    return (
      <div className="rounded-xl border border-dashed border-line p-8 text-sm text-zinc-500">
        <p>{error}</p>
        <Link href="/" className="pt-2 text-sky-600 underline">
          Back to projects
        </Link>
      </div>
    );
  }

  if (!detail) {
    return <p className="text-sm text-zinc-500">Loading service…</p>;
  }

  const style = healthStyle[detail.health];
  const cpu = detail.instances.reduce((sum, i) => sum + i.cpu_percent, 0);
  const memory = detail.instances.reduce((sum, i) => sum + i.memory_bytes, 0);
  const nameOf = (serviceId: string) =>
    services.find((service) => service.id === serviceId)?.name ?? serviceId.slice(0, 12);

  return (
    <div className="flex flex-col gap-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <div>
          <div className="flex items-center gap-2">
            {detail.project_id && (
              <>
                <Link
                  href={`/projects/${detail.project_id}`}
                  className="text-sm text-zinc-500 hover:underline"
                >
                  project
                </Link>
                <span className="text-zinc-400">/</span>
              </>
            )}
            <h1 className="text-xl font-semibold">{detail.name}</h1>
            <span className={`flex items-center gap-1.5 text-xs ${style.text}`}>
              <HealthDot health={detail.health} />
              {style.label}
            </span>
          </div>
          <p className="pt-1 text-xs text-zinc-500">
            {detail.kind.kind === "container" ? "container" : "host process"} ·{" "}
            {detail.runtime} · first seen {ago(detail.first_seen)} ·{" "}
            {detail.restart_count} restarts
          </p>
        </div>

        <dl className="flex gap-6 text-sm">
          <Stat label="cpu" value={percent(cpu)} />
          <Stat label="memory" value={bytes(memory)} />
          <Stat label="processes" value={String(detail.instances.length)} />
          <Stat label="ports" value={String(detail.endpoints.length)} />
        </dl>
      </header>

      <div className="grid gap-6 lg:grid-cols-2">
        <Panel title="Processes">
          {detail.instances.length === 0 ? (
            <p className="px-3 py-4 text-sm text-zinc-500">
              {detail.kind.kind === "container"
                ? "Docker does not disclose the host PIDs of a container's processes."
                : "Not running."}
            </p>
          ) : (
            <ul className="divide-y divide-line">
              {detail.instances.map((instance) => (
                <li key={instance.pid} className="flex flex-col gap-1 px-3 py-2 text-sm">
                  <div className="flex items-baseline gap-3">
                    <span className="font-mono text-xs text-zinc-500">
                      pid {instance.pid}
                    </span>
                    <span className="tabular-nums">
                      {percent(instance.cpu_percent)} · {bytes(instance.memory_bytes)}
                    </span>
                    <span className="ml-auto text-xs text-zinc-400">
                      started {ago(instance.started_at)}
                    </span>
                  </div>
                  {instance.cwd && (
                    <p className="truncate font-mono text-xs text-zinc-500" title={instance.cwd}>
                      {instance.cwd}
                    </p>
                  )}
                  {instance.command.length > 0 && (
                    <p className="truncate font-mono text-xs text-zinc-500">
                      {instance.command.join(" ")}
                    </p>
                  )}
                </li>
              ))}
            </ul>
          )}
        </Panel>

        <Panel title="Ports">
          {detail.endpoints.length === 0 ? (
            <p className="px-3 py-4 text-sm text-zinc-500">Listening on nothing.</p>
          ) : (
            <ul className="divide-y divide-line">
              {detail.endpoints.map((endpoint) => (
                <li
                  key={`${endpoint.address}:${endpoint.port}/${endpoint.protocol}`}
                  className="flex items-baseline gap-3 px-3 py-2 font-mono text-sm"
                >
                  <span>
                    {endpoint.address}:{endpoint.port}
                  </span>
                  <span className="text-xs text-zinc-500">{endpoint.protocol}</span>
                </li>
              ))}
            </ul>
          )}
        </Panel>

        <Panel title="Connections">
          <Edges
            label="calls"
            edges={detail.connections.outbound}
            other={(edge) => edge.target}
            nameOf={nameOf}
          />
          <Edges
            label="called by"
            edges={detail.connections.inbound}
            other={(edge) => edge.source}
            nameOf={nameOf}
          />
          {detail.connections.outbound.length === 0 &&
            detail.connections.inbound.length === 0 && (
              <p className="px-3 py-4 text-sm text-zinc-500">
                No connections observed.
              </p>
            )}
        </Panel>

        <Panel title="Resources">
          <Sparkline samples={detail.resource_history ?? []} />
        </Panel>
      </div>

      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-medium text-zinc-500">Recent events</h2>
        <div className="rounded-lg border border-line bg-surface-raised px-3 py-1">
          <Timeline events={detail.recent_events} services={services} />
        </div>
      </section>

      <p className="font-mono text-xs text-zinc-400" title="Stable identity, independent of PID">
        {detail.fingerprint}
      </p>
    </div>
  );
}

function Edges({
  label,
  edges,
  other,
  nameOf,
}: {
  label: string;
  edges: Connection[];
  other: (edge: Connection) => string;
  nameOf: (id: string) => string;
}) {
  if (edges.length === 0) return null;

  return (
    <div className="px-3 py-2">
      <h3 className="text-xs text-zinc-500">{label}</h3>
      <ul className="pt-1">
        {edges.map((edge) => {
          const certain =
            edge.evidence.evidence_type === "observed_socket" ||
            edge.evidence.evidence_type === "otel_span";
          return (
            <li key={edge.id} className="flex items-baseline gap-2 text-sm">
              <Link href={`/services/${other(edge)}`} className="hover:underline">
                {nameOf(other(edge))}
              </Link>
              <span className="font-mono text-xs text-zinc-500">:{edge.target_port}</span>
              <span
                className={`text-xs ${certain ? "text-zinc-400" : "text-amber-600"}`}
                title={edge.evidence.detail ?? undefined}
              >
                {edge.evidence.evidence_type} ·{" "}
                {(edge.evidence.confidence * 100).toFixed(0)}%
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * CPU and memory over the retained window. Drawn from the samples the daemon
 * kept — no interpolation, no smoothing, so a gap looks like a gap.
 */
function Sparkline({ samples }: { samples: ResourceSample[] }) {
  if (samples.length < 2) {
    return (
      <p className="px-3 py-4 text-sm text-zinc-500">
        Not enough samples yet.
      </p>
    );
  }

  const width = 480;
  const height = 96;
  const maxCpu = Math.max(...samples.map((s) => s.cpu_percent), 1);
  const maxMemory = Math.max(...samples.map((s) => s.memory_bytes), 1);

  const path = (pick: (sample: ResourceSample) => number, max: number) =>
    samples
      .map((sample, index) => {
        const x = (index / (samples.length - 1)) * width;
        const y = height - (pick(sample) / max) * (height - 8) - 4;
        return `${index === 0 ? "M" : "L"} ${x.toFixed(1)} ${y.toFixed(1)}`;
      })
      .join(" ");

  const last = samples[samples.length - 1];

  return (
    <div className="px-3 py-2">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="w-full"
        role="img"
        aria-label="CPU and memory over time"
      >
        <path d={path((s) => s.cpu_percent, maxCpu)} fill="none" strokeWidth={1.5} className="stroke-sky-500" />
        <path
          d={path((s) => s.memory_bytes, maxMemory)}
          fill="none"
          strokeWidth={1.5}
          className="stroke-violet-500"
        />
      </svg>
      <p className="pt-1 text-xs text-zinc-500">
        <span className="text-sky-600">cpu</span> peak {percent(maxCpu)} ·{" "}
        <span className="text-violet-600">memory</span> peak {bytes(maxMemory)} ·{" "}
        {samples.length} samples to {last ? clock(last.at) : "—"}
      </p>
    </div>
  );
}

function Panel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <h2 className="text-sm font-medium text-zinc-500">{title}</h2>
      <div className="rounded-lg border border-line bg-surface-raised">{children}</div>
    </section>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-zinc-500">{label}</dt>
      <dd className="font-medium tabular-nums">{value}</dd>
    </div>
  );
}
