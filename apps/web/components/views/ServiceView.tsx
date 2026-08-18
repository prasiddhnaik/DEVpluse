"use client";

/**
 * Service inspector (task T4.5): processes, cwd, redacted command, ports,
 * resources, connections and recent events.
 *
 * The command line shown here was redacted by the daemon at capture time — the
 * dashboard has never seen the raw argv, and cannot un-redact it
 * (`AGENTS.md` rule 6).
 */

import { useEffect, useState, type ReactNode } from "react";

import { HealthDot } from "@/components/HealthDot";
import { NavLink } from "@/components/NavLink";
import { SeriesChart, type SeriesPoint } from "@/components/SeriesChart";
import { Timeline } from "@/components/Timeline";
import { api } from "@/lib/api";
import { useDaemon } from "@/lib/daemon";
import { ago, bytes, bytesPerTick, healthStyle, percent } from "@/lib/format";
import type { Connection, ResourceSample, ServiceDetail } from "@/lib/types";

/** How often the inspector re-reads the detail endpoint while it is open. */
const REFRESH_MS = 2_000;

export function ServiceView({ id }: { id: string }) {
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
        <NavLink href="/" className="pt-2 text-accent underline">
          Back to projects
        </NavLink>
      </div>
    );
  }

  if (!detail) {
    return <p className="text-sm text-zinc-500">Loading service…</p>;
  }

  const style = healthStyle[detail.health];
  const cpu = detail.instances.reduce((sum, i) => sum + i.cpu_percent, 0);
  const memory = detail.instances.reduce((sum, i) => sum + i.memory_bytes, 0);
  const samples = detail.resource_history ?? [];
  const nameOf = (serviceId: string) =>
    services.find((service) => service.id === serviceId)?.name ?? serviceId.slice(0, 12);

  return (
    <div className="flex min-w-0 flex-col gap-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <div>
          <div className="flex items-center gap-2">
            {detail.project_id && (
              <>
                <NavLink
                  href={`/projects/${detail.project_id}`}
                  className="text-sm text-zinc-500 hover:underline"
                >
                  project
                </NavLink>
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

      <div className="grid min-w-0 gap-6 lg:grid-cols-2">
        <Panel title="Processes" fill>
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

        <Panel title="Recent events" fill padded>
          <Timeline events={detail.recent_events} services={services} />
        </Panel>
      </div>

      <section className="flex min-w-0 flex-col gap-3">
        <h2 className="text-sm font-medium text-zinc-500">Resources</h2>
        <div className="grid min-w-0 gap-4 sm:grid-cols-2">
          <ChartPanel title="CPU">
            <SeriesChart
              points={series(samples, (s) => s.cpu_percent)}
              format={percent}
              strokeClass="stroke-sky-500"
              fillClass="fill-sky-500"
              label="cpu"
            />
          </ChartPanel>
          <ChartPanel title="RAM (RSS)">
            <SeriesChart
              points={series(samples, (s) => s.memory_bytes)}
              format={bytes}
              strokeClass="stroke-violet-500"
              fillClass="fill-violet-500"
              label="rss"
            />
          </ChartPanel>
          <ChartPanel title="Virtual memory">
            <SeriesChart
              points={series(samples, (s) => s.virtual_memory_bytes)}
              format={bytes}
              strokeClass="stroke-fuchsia-500"
              fillClass="fill-fuchsia-500"
              label="virtual"
            />
          </ChartPanel>
          <ChartPanel title="Threads">
            <SeriesChart
              points={series(samples, (s) => s.thread_count)}
              format={(value) => String(Math.round(value))}
              strokeClass="stroke-amber-500"
              fillClass="fill-amber-500"
              label="threads"
            />
          </ChartPanel>
          <ChartPanel title="Disk read (per tick)">
            <SeriesChart
              points={series(samples, (s) => s.disk_read_bytes)}
              format={bytesPerTick}
              strokeClass="stroke-emerald-500"
              fillClass="fill-emerald-500"
              label="disk read"
              caption="delta since previous sample"
            />
          </ChartPanel>
          <ChartPanel title="Disk write (per tick)">
            <SeriesChart
              points={series(samples, (s) => s.disk_write_bytes)}
              format={bytesPerTick}
              strokeClass="stroke-lime-500"
              fillClass="fill-lime-500"
              label="disk write"
              caption="delta since previous sample"
            />
          </ChartPanel>
          <ChartPanel title="Observed connections">
            <SeriesChart
              points={series(samples, (s) => s.connection_count)}
              format={(value) => String(Math.round(value))}
              strokeClass="stroke-cyan-500"
              fillClass="fill-cyan-500"
              label="edges"
              caption="topology edges this tick, not network bytes"
            />
          </ChartPanel>
        </div>
      </section>

      <div className="grid min-w-0 gap-6 lg:grid-cols-2">
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
      </div>

      <p className="font-mono text-xs text-zinc-400" title="Stable identity, independent of PID">
        {detail.fingerprint}
      </p>
    </div>
  );
}

function series(
  samples: ResourceSample[],
  pick: (sample: ResourceSample) => number,
): SeriesPoint[] {
  return samples.map((sample) => ({ at: sample.at, value: pick(sample) }));
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
              <NavLink href={`/services/${other(edge)}`} className="hover:underline">
                {nameOf(other(edge))}
              </NavLink>
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

const PANE_HEIGHT = "h-[min(28rem,calc(100dvh-14rem))]";

function Panel({
  title,
  children,
  fill = false,
  padded = false,
}: {
  title: string;
  children: ReactNode;
  fill?: boolean;
  padded?: boolean;
}) {
  return (
    <section className="flex min-w-0 flex-col gap-2">
      <h2 className="text-sm font-medium text-zinc-500">{title}</h2>
      <div
        className={`min-w-0 overflow-auto rounded-lg border border-line bg-surface-raised ${
          fill ? PANE_HEIGHT : ""
        } ${padded ? "px-3 py-1" : ""}`}
      >
        {children}
      </div>
    </section>
  );
}

function ChartPanel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-col gap-2">
      <h3 className="text-xs font-medium text-zinc-500">{title}</h3>
      <div className="min-w-0 overflow-hidden rounded-lg border border-line bg-surface-raised">
        {children}
      </div>
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
