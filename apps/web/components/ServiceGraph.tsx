"use client";

/**
 * Project topology (task T4.4).
 *
 * Hand-drawn SVG rather than a graph library: the data contract is what matters
 * here, and a layered layout of ten to thirty nodes needs no physics
 * (`AGENTS.md` preferred stack — "a graph library only after the graph data
 * contract is stable").
 *
 * Layout is deterministic: callers depend on nodes not moving between ticks,
 * because a node that jumps every second is unreadable.
 *
 * Evidence is rendered, never hidden (`AGENTS.md` rule 4): an edge that is
 * inferred or below full confidence is dashed and labelled, so it can never be
 * mistaken for something DevPulse actually observed.
 */

import { useMemo } from "react";

import { bytes, healthStyle, percent } from "@/lib/format";
import type { Connection, Service } from "@/lib/types";

const NODE_WIDTH = 176;
const NODE_HEIGHT = 62;
const COLUMN_GAP = 96;
const ROW_GAP = 24;
const PADDING = 16;

interface Placed {
  service: Service;
  x: number;
  y: number;
  column: number;
}

export function ServiceGraph({
  services,
  connections,
  selectedId,
}: {
  services: Service[];
  connections: Connection[];
  selectedId?: string;
}) {
  const { placed, width, height } = useMemo(
    () => layout(services, connections),
    [services, connections],
  );

  if (services.length === 0) {
    return (
      <p className="rounded-lg border border-dashed border-line p-6 text-sm text-zinc-500">
        No services observed in this project yet.
      </p>
    );
  }

  const byId = new Map(placed.map((node) => [node.service.id, node]));

  return (
    <div className="overflow-x-auto rounded-lg border border-line bg-surface-raised p-2">
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label="Service topology"
      >
        <defs>
          <marker
            id="devpulse-arrow"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="6"
            markerHeight="6"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 10 5 L 0 10 z" className="fill-zinc-400" />
          </marker>
        </defs>

        {connections.map((connection) => {
          const source = byId.get(connection.source);
          const target = byId.get(connection.target);
          if (!source || !target) return null;

          const certain =
            connection.evidence.evidence_type === "observed_socket" ||
            connection.evidence.evidence_type === "otel_span";
          const solid = certain && connection.evidence.confidence >= 1;

          const x1 = source.x + NODE_WIDTH;
          const y1 = source.y + NODE_HEIGHT / 2;
          const x2 = target.x;
          const y2 = target.y + NODE_HEIGHT / 2;
          const midX = (x1 + x2) / 2;

          return (
            <g key={connection.id}>
              <path
                d={`M ${x1} ${y1} C ${midX} ${y1}, ${midX} ${y2}, ${x2} ${y2}`}
                fill="none"
                strokeWidth={1.5}
                strokeDasharray={solid ? undefined : "5 4"}
                className={solid ? "stroke-zinc-400" : "stroke-amber-500"}
                markerEnd="url(#devpulse-arrow)"
              >
                {/* One string: React renders `<title>` children as text and
                    cannot join an array, so a split title silently renders
                    empty — and an edge with no visible evidence is exactly
                    what `AGENTS.md` rule 4 forbids. */}
                <title>{evidenceLabel(connection.evidence)}</title>
              </path>
              <text
                x={midX}
                y={(y1 + y2) / 2 - 6}
                textAnchor="middle"
                className="fill-zinc-500 text-[10px]"
              >
                :{connection.target_port}
                {solid ? "" : ` · ${connection.evidence.evidence_type}`}
              </text>
            </g>
          );
        })}

        {placed.map(({ service, x, y }) => {
          const style = healthStyle[service.health];
          const port = service.endpoints[0]?.port ?? null;
          const cpu = service.instances.reduce((sum, i) => sum + i.cpu_percent, 0);
          const memory = service.instances.reduce((sum, i) => sum + i.memory_bytes, 0);
          const selected = service.id === selectedId;

          return (
            <a key={service.id} href={`/services/${service.id}`}>
              <g transform={`translate(${x} ${y})`} className="cursor-pointer">
                <rect
                  width={NODE_WIDTH}
                  height={NODE_HEIGHT}
                  rx={10}
                  className={`fill-surface stroke-line ${selected ? "stroke-sky-500" : ""}`}
                  strokeWidth={selected ? 2 : 1}
                />
                <circle cx={16} cy={20} r={4} className={style.svg} />
                <text
                  x={28}
                  y={24}
                  className="fill-zinc-900 text-[13px] font-medium dark:fill-zinc-100"
                >
                  {truncate(service.name, 18)}
                </text>
                <text x={16} y={42} className="fill-zinc-500 text-[11px]">
                  {service.runtime}
                  {port ? ` · :${port}` : ""}
                </text>
                <text x={16} y={55} className="fill-zinc-500 text-[11px]">
                  {percent(cpu)} · {bytes(memory)}
                </text>
              </g>
            </a>
          );
        })}
      </svg>
    </div>
  );
}

/**
 * Layered layout: a service that nothing calls goes in the first column, and
 * everything it calls goes to its right. Cycles are broken by keeping the first
 * column a service lands in, so the picture stays stable.
 */
function layout(services: Service[], connections: Connection[]) {
  const present = new Set(services.map((service) => service.id));
  const edges = connections.filter(
    (connection) => present.has(connection.source) && present.has(connection.target),
  );

  const depth = new Map<string, number>();
  const order = [...services].sort((a, b) => a.name.localeCompare(b.name));
  for (const service of order) depth.set(service.id, 0);

  // Longest-path layering, bounded so a cycle cannot spin here.
  for (let pass = 0; pass < services.length; pass += 1) {
    let moved = false;
    for (const edge of edges) {
      const source = depth.get(edge.source) ?? 0;
      const target = depth.get(edge.target) ?? 0;
      if (target < source + 1) {
        depth.set(edge.target, source + 1);
        moved = true;
      }
    }
    if (!moved) break;
  }

  const columns = new Map<number, Service[]>();
  for (const service of order) {
    const column = depth.get(service.id) ?? 0;
    const bucket = columns.get(column) ?? [];
    bucket.push(service);
    columns.set(column, bucket);
  }

  const placed: Placed[] = [];
  for (const [column, bucket] of [...columns.entries()].sort((a, b) => a[0] - b[0])) {
    bucket.forEach((service, row) => {
      placed.push({
        service,
        column,
        x: PADDING + column * (NODE_WIDTH + COLUMN_GAP),
        y: PADDING + row * (NODE_HEIGHT + ROW_GAP),
      });
    });
  }

  const width =
    PADDING * 2 + (columns.size - 1) * (NODE_WIDTH + COLUMN_GAP) + NODE_WIDTH;
  const tallest = Math.max(...[...columns.values()].map((bucket) => bucket.length), 1);
  const height = PADDING * 2 + tallest * NODE_HEIGHT + (tallest - 1) * ROW_GAP;

  return { placed, width: Math.max(width, 320), height };
}

/** The evidence behind an edge, in one line, for the hover title. */
function evidenceLabel(evidence: Connection["evidence"]): string {
  const base = `${evidence.evidence_type}, confidence ${evidence.confidence.toFixed(2)}`;
  return evidence.detail ? `${base} — ${evidence.detail}` : base;
}

function truncate(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, max - 1)}…`;
}
