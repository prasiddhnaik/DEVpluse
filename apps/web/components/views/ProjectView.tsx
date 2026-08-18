"use client";

/**
 * One project: its graph (T4.4), its services, its warnings and its timeline
 * (T4.6).
 *
 * Live data comes from the WebSocket view; the event history for this project
 * is fetched once and then extended by the live stream, so opening the page
 * does not start the timeline from zero.
 */

import { useCallback, useEffect, useMemo, useState } from "react";

import { HealthDot } from "@/components/HealthDot";
import { NavLink } from "@/components/NavLink";
import { SeriesChart, type SeriesPoint } from "@/components/SeriesChart";
import { ServiceGraph } from "@/components/ServiceGraph";
import { Timeline } from "@/components/Timeline";
import { WarningBanner } from "@/components/WarningBanner";
import { api } from "@/lib/api";
import { useDaemon, useProject, useProjectServices } from "@/lib/daemon";
import { ago, bytes, healthStyle, percent, shortenPath } from "@/lib/format";
import { navigate } from "@/lib/route";
import type { ResourceSample, RunscapeEvent } from "@/lib/types";

/** One viewport band so services, charts, and timeline stay on screen together. */
const PANE =
  "h-[min(28rem,calc(100dvh-14rem))] min-h-0 min-w-0 overflow-auto rounded-lg border border-line bg-surface-raised";

const REFRESH_MS = 2_000;

export function ProjectView({ id }: { id: string }) {
  const project = useProject(id);
  const services = useProjectServices(id);
  const { connection, connections, warnings, events: liveEvents } = useDaemon();
  const openService = useCallback((serviceId: string) => {
    navigate(`/services/${serviceId}`);
  }, []);

  const [history, setHistory] = useState<RunscapeEvent[]>([]);
  const [samples, setSamples] = useState<ResourceSample[]>([]);

  useEffect(() => {
    const controller = new AbortController();
    api
      .events({ projectId: id, limit: 200 }, controller.signal)
      .then(setHistory)
      .catch(() => setHistory([]));
    return () => controller.abort();
  }, [id]);

  useEffect(() => {
    const controller = new AbortController();
    const load = () =>
      api
        .project(id, controller.signal)
        .then((detail) => setSamples(detail.resource_history ?? []))
        .catch(() => {
          /* keep the last successful series; the live lists still update */
        });
    void load();
    const timer = setInterval(() => void load(), REFRESH_MS);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [id]);

  const serviceIds = useMemo(
    () => new Set(services.map((service) => service.id)),
    [services],
  );

  const edges = useMemo(
    () =>
      connections.filter(
        (connection) =>
          serviceIds.has(connection.source) || serviceIds.has(connection.target),
      ),
    [connections, serviceIds],
  );

  const projectWarnings = warnings.filter((warning) => warning.project_id === id);

  const events = useMemo(() => {
    const mine = liveEvents.filter((event) => event.project_id === id);
    const seen = new Set(mine.map((event) => event.id));
    return [...mine, ...history.filter((event) => !seen.has(event.id))].slice(0, 200);
  }, [liveEvents, history, id]);

  if (!project) {
    if (connection === "connecting" || connection === "reconnecting") {
      return <p className="text-sm text-zinc-500">Loading project…</p>;
    }
    return (
      <div className="rounded-xl border border-dashed border-line p-8 text-sm text-zinc-500">
        <p>This project is not currently running.</p>
        <NavLink href="/" className="pt-2 text-accent underline">
          Back to projects
        </NavLink>
      </div>
    );
  }

  const style = healthStyle[project.health];

  return (
    <div className="flex flex-col gap-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <div>
          <div className="flex items-center gap-2">
            <NavLink href="/" className="text-sm text-zinc-500 hover:underline">
              projects
            </NavLink>
            <span className="text-zinc-400">/</span>
            <h1 className="text-xl font-semibold">{project.name}</h1>
            <span className={`flex items-center gap-1.5 text-xs ${style.text}`}>
              <HealthDot health={project.health} />
              {style.label}
            </span>
          </div>
          <p className="pt-1 font-mono text-xs text-zinc-500" title={project.root}>
            {project.root}
          </p>
        </div>

        <dl className="flex gap-6 text-sm">
          <Stat
            label="services"
            value={`${project.running_service_count}/${project.service_count}`}
          />
          <Stat label="memory" value={bytes(project.memory_bytes)} />
          <Stat label="cpu" value={percent(project.cpu_percent)} />
          <Stat label="edges" value={String(edges.length)} />
        </dl>
      </header>

      <WarningBanner warnings={projectWarnings} />

      <div className="grid min-w-0 grid-cols-1 gap-6 lg:grid-cols-[minmax(16rem,1fr)_minmax(0,1.5fr)_minmax(18rem,1fr)] lg:items-stretch">
        <section className="flex min-w-0 flex-col gap-2">
          <h2 className="text-sm font-medium text-zinc-500">Services</h2>
          <ul className={`${PANE} divide-y divide-line`}>
            {services.map((service) => (
              <li key={service.id}>
                <NavLink
                  href={`/services/${service.id}`}
                  className="flex items-baseline gap-3 px-3 py-2 text-sm transition hover:bg-surface"
                >
                  <HealthDot health={service.health} className="translate-y-0.5" />
                  <span className="font-medium">{service.name}</span>
                  {service.runtime !== service.name && (
                    <span className="text-xs text-zinc-500">{service.runtime}</span>
                  )}
                  {service.endpoints[0] && (
                    <span className="font-mono text-xs text-zinc-500">
                      :{service.endpoints[0].port}
                    </span>
                  )}
                  {service.restart_count > 0 && (
                    <span className="text-xs text-amber-600">
                      {service.restart_count} restarts
                    </span>
                  )}
                  <span className="ml-auto text-xs text-zinc-400">
                    {ago(service.last_seen)}
                  </span>
                </NavLink>
              </li>
            ))}
            {services.length === 0 && (
              <li className="px-3 py-6 text-sm text-zinc-500">
                No services are running in this project right now.
              </li>
            )}
          </ul>
        </section>

        <section className="flex min-w-0 flex-col gap-2">
          <h2 className="text-sm font-medium text-zinc-500">CPU and memory</h2>
          <div className={`${PANE} flex flex-col gap-3 p-3`}>
            <div className="min-w-0">
              <h3 className="pb-1 text-xs text-zinc-500">CPU</h3>
              <SeriesChart
                points={series(samples, (sample) => sample.cpu_percent)}
                format={percent}
                strokeClass="stroke-sky-500"
                fillClass="fill-sky-500"
                label="cpu"
                empty="Waiting for the first two samples from this project's services."
              />
            </div>
            <div className="min-w-0">
              <h3 className="pb-1 text-xs text-zinc-500">RAM (RSS)</h3>
              <SeriesChart
                points={series(samples, (sample) => sample.memory_bytes)}
                format={bytes}
                strokeClass="stroke-violet-500"
                fillClass="fill-violet-500"
                label="rss"
                empty="Waiting for the first two samples from this project's services."
              />
            </div>
          </div>
        </section>

        <section className="flex min-w-0 flex-col gap-2">
          <h2 className="text-sm font-medium text-zinc-500">Timeline</h2>
          <div className={`${PANE} px-3 py-1`}>
            <Timeline events={events} services={services} />
          </div>
        </section>
      </div>

      <section className="flex min-w-0 flex-col gap-2">
        <h2 className="text-sm font-medium text-zinc-500">Topology</h2>
        <div className="min-h-0 min-w-0 max-h-80 overflow-auto rounded-lg border border-line bg-surface-raised">
          <ServiceGraph
            services={services}
            connections={edges}
            onOpenService={openService}
            className="w-full overflow-x-auto p-2"
          />
        </div>
        <p className="text-xs text-zinc-500">
          Solid edges were observed in the kernel&apos;s socket tables. Dashed
          edges are inferred or below full confidence — hover one to see the
          evidence behind it.
        </p>
      </section>

      <p className="text-xs text-zinc-500">
        Root resolved with confidence {project.confidence.toFixed(2)} ·{" "}
        {shortenPath(project.root, 2)} · first seen {ago(project.first_seen)}
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

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-zinc-500">{label}</dt>
      <dd className="font-medium tabular-nums">{value}</dd>
    </div>
  );
}
