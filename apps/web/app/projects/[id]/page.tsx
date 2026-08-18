"use client";

/**
 * One project: its graph (T4.4), its services, its warnings and its timeline
 * (T4.6).
 *
 * Live data comes from the WebSocket view; the event history for this project
 * is fetched once and then extended by the live stream, so opening the page
 * does not start the timeline from zero.
 */

import Link from "next/link";
import { use, useEffect, useMemo, useState } from "react";

import { HealthDot } from "@/components/HealthDot";
import { ServiceGraph } from "@/components/ServiceGraph";
import { Timeline } from "@/components/Timeline";
import { WarningBanner } from "@/components/WarningBanner";
import { api } from "@/lib/api";
import { useDaemon, useProject, useProjectServices } from "@/lib/daemon";
import { ago, bytes, healthStyle, percent, shortenPath } from "@/lib/format";
import type { DevPulseEvent } from "@/lib/types";

export default function ProjectPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  const project = useProject(id);
  const services = useProjectServices(id);
  const { connections, warnings, events: liveEvents } = useDaemon();

  const [history, setHistory] = useState<DevPulseEvent[]>([]);

  useEffect(() => {
    const controller = new AbortController();
    api
      .events({ projectId: id, limit: 200 }, controller.signal)
      .then(setHistory)
      .catch(() => setHistory([]));
    return () => controller.abort();
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

  // Live events first, then the fetched history, deduplicated by id.
  const events = useMemo(() => {
    const mine = liveEvents.filter((event) => event.project_id === id);
    const seen = new Set(mine.map((event) => event.id));
    return [...mine, ...history.filter((event) => !seen.has(event.id))].slice(0, 200);
  }, [liveEvents, history, id]);

  if (!project) {
    return (
      <div className="rounded-xl border border-dashed border-line p-8 text-sm text-zinc-500">
        <p>This project is not currently running.</p>
        <Link href="/" className="pt-2 text-sky-600 underline">
          Back to projects
        </Link>
      </div>
    );
  }

  const style = healthStyle[project.health];

  return (
    <div className="flex flex-col gap-6">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <div>
          <div className="flex items-center gap-2">
            <Link href="/" className="text-sm text-zinc-500 hover:underline">
              projects
            </Link>
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

      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-medium text-zinc-500">Topology</h2>
        <ServiceGraph services={services} connections={edges} />
        <p className="text-xs text-zinc-500">
          Solid edges were observed in the kernel&apos;s socket tables. Dashed
          edges are inferred or below full confidence — hover one to see the
          evidence behind it.
        </p>
      </section>

      <div className="grid gap-6 lg:grid-cols-2">
        <section className="flex flex-col gap-2">
          <h2 className="text-sm font-medium text-zinc-500">Services</h2>
          <ul className="divide-y divide-line rounded-lg border border-line bg-surface-raised">
            {services.map((service) => (
              <li key={service.id}>
                <Link
                  href={`/services/${service.id}`}
                  className="flex items-baseline gap-3 px-3 py-2 text-sm transition hover:bg-surface"
                >
                  <HealthDot health={service.health} className="translate-y-0.5" />
                  <span className="font-medium">{service.name}</span>
                  <span className="text-xs text-zinc-500">{service.runtime}</span>
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
                </Link>
              </li>
            ))}
            {services.length === 0 && (
              <li className="px-3 py-6 text-sm text-zinc-500">
                No services are running in this project right now.
              </li>
            )}
          </ul>
        </section>

        <section className="flex flex-col gap-2">
          <h2 className="text-sm font-medium text-zinc-500">Timeline</h2>
          <div className="rounded-lg border border-line bg-surface-raised px-3 py-1">
            <Timeline events={events} services={services} />
          </div>
        </section>
      </div>

      <p className="text-xs text-zinc-500">
        Root resolved with confidence {project.confidence.toFixed(2)} ·{" "}
        {shortenPath(project.root, 2)} · first seen {ago(project.first_seen)}
      </p>
    </div>
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
