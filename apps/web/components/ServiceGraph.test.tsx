/**
 * The graph is where `AGENTS.md` rule 4 is either honoured or broken: an
 * inferred edge must never look like an observed one. That is asserted here on
 * the rendered SVG, not on a prop.
 */

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { ServiceGraph } from "./ServiceGraph";
import type { Connection, Evidence, Service } from "@/lib/types";

function service(id: string, name: string, port: number | null): Service {
  return {
    id,
    project_id: "prj_1",
    name,
    kind: { kind: "host_process" },
    runtime: "node",
    fingerprint: `host|${name}`,
    health: "healthy",
    restart_count: 0,
    first_seen: "2026-08-18T10:00:00Z",
    last_seen: "2026-08-18T10:00:10Z",
    instances: [
      {
        pid: 100,
        parent_pid: 1,
        executable: "/usr/bin/node",
        command: ["node", "server.js"],
        cwd: "/tmp/app",
        started_at: "2026-08-18T10:00:00Z",
        cpu_percent: 1.5,
        memory_bytes: 40 * 1024 * 1024,
      },
    ],
    endpoints: port
      ? [{ address: "127.0.0.1", port, protocol: "tcp", pid: 100 }]
      : [],
  };
}

function edge(source: string, target: string, evidence: Evidence): Connection {
  return {
    id: `con_${source}_${target}`,
    source,
    target,
    target_port: 5432,
    evidence,
  };
}

const observed: Evidence = {
  evidence_type: "observed_socket",
  confidence: 1,
  first_seen: "2026-08-18T10:00:00Z",
  last_seen: "2026-08-18T10:00:10Z",
  detail: null,
};

const inferred: Evidence = {
  evidence_type: "inferred",
  confidence: 0.5,
  first_seen: "2026-08-18T10:00:00Z",
  last_seen: "2026-08-18T10:00:10Z",
  detail: "same compose network",
};

describe("ServiceGraph", () => {
  test("draws a node per service with its runtime and port", () => {
    const html = renderToStaticMarkup(
      <ServiceGraph
        services={[service("svc_a", "web", 3000), service("svc_b", "db", 5432)]}
        connections={[]}
      />,
    );

    expect(html).toContain("web");
    expect(html).toContain("db");
    expect(html).toContain(":3000");
    expect(html).toContain("node");
    expect(html).toContain('href="/services/svc_a"');
    expect(html).toContain("fill-emerald-500");
    expect(html).not.toContain("min-w-full");
  });

  test("isolated services wrap into a grid instead of one tall column", () => {
    const services = Array.from({ length: 10 }, (_, index) =>
      service(`svc_${index}`, `s${index}`, null),
    );
    const html = renderToStaticMarkup(
      <ServiceGraph services={services} connections={[]} />,
    );

    const width = Number(/width="(\d+)"/.exec(html)?.[1]);
    const height = Number(/height="(\d+)"/.exec(html)?.[1]);
    expect(width).toBe(1024);
    expect(height).toBe(266);
  });

  test("an observed edge is solid and an inferred one is not", () => {
    const services = [service("svc_a", "web", 3000), service("svc_b", "db", 5432)];

    const certain = renderToStaticMarkup(
      <ServiceGraph services={services} connections={[edge("svc_a", "svc_b", observed)]} />,
    );
    const uncertain = renderToStaticMarkup(
      <ServiceGraph services={services} connections={[edge("svc_a", "svc_b", inferred)]} />,
    );

    expect(certain).not.toContain("stroke-dasharray");
    expect(uncertain).toContain("stroke-dasharray");
    // An uncertain edge names its evidence type on the canvas, not just in a
    // tooltip, so it cannot be mistaken for an observation.
    expect(uncertain).toContain("inferred");
    expect(uncertain).toContain("same compose network");
  });

  test("an edge to a service outside the project is not drawn as a dangling line", () => {
    const html = renderToStaticMarkup(
      <ServiceGraph
        services={[service("svc_a", "web", 3000)]}
        connections={[edge("svc_a", "svc_elsewhere", observed)]}
      />,
    );

    expect(html).toContain("web");
    // The only `marker-end` in the output belongs to an edge; the arrowhead
    // marker definition itself has none.
    expect(html).not.toContain("marker-end");
  });

  test("says so plainly when there is nothing to draw", () => {
    const html = renderToStaticMarkup(<ServiceGraph services={[]} connections={[]} />);
    expect(html).toContain("No services observed");
  });

  test("layout is stable: the same input renders identically", () => {
    const services = [service("svc_b", "db", 5432), service("svc_a", "web", 3000)];
    const connections = [edge("svc_a", "svc_b", observed)];

    const first = renderToStaticMarkup(
      <ServiceGraph services={services} connections={connections} />,
    );
    const second = renderToStaticMarkup(
      <ServiceGraph services={[...services].reverse()} connections={connections} />,
    );

    expect(first).toBe(second);
  });
});
