/**
 * Home tile: mock B without a sparkline. Idle cards must not print zeros.
 */

import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { ProjectCard, isProjectIdle, projectStateLabel } from "./ProjectCard";
import type { ProjectSummary, RankedService, Warning } from "@/lib/types";

const WARNING: Warning = {
  id: "warn_1",
  rule: "memory_growth",
  severity: "warning",
  project_id: "prj_1",
  service_id: "svc_python",
  message: "python memory grew from 12MB to 26MB without falling back",
  first_seen: "2026-08-18T10:00:00Z",
  last_seen: "2026-08-18T10:00:10Z",
  related_events: [],
};

function ranked(
  id: string,
  name: string,
  cpu: number,
  memory: number,
  share: number,
): RankedService {
  return {
    id,
    name,
    cpu_percent: cpu,
    memory_bytes: memory,
    share,
    process_count: 1,
    resources_measured: true,
  };
}

function project(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    id: "prj_1",
    name: "DEVpluse",
    root: "/Users/prasiddhnaik/Documents/DEVpluse",
    kind: "git_repository",
    confidence: 1,
    first_seen: "2026-08-18T09:00:00Z",
    last_seen: "2026-08-18T10:00:10Z",
    service_count: 4,
    running_service_count: 4,
    health: "healthy",
    memory_bytes: 45 * 1024 * 1024,
    cpu_percent: 4.4,
    recent_warning: WARNING,
    ranked_by_cpu: [
      ranked("svc_runscape", "runscape", 3.7, 21 * 1024 * 1024, 0.84),
      ranked("svc_python", "python", 0.7, 26 * 1024 * 1024, 0.16),
    ],
    ranked_by_memory: [
      ranked("svc_python", "python", 0.7, 26 * 1024 * 1024, 0.58),
      ranked("svc_runscape", "runscape", 3.7, 21 * 1024 * 1024, 0.47),
    ],
    dominant_memory: { id: "svc_python", name: "python", share: 0.58 },
    ...overrides,
  };
}

describe("project card state", () => {
  test("idle means nothing is running, even if services were seen", () => {
    const idle = project({ running_service_count: 0, health: "unknown" });
    expect(isProjectIdle(idle)).toBe(true);
    expect(projectStateLabel(idle)).toBe("not running");
  });

  test("a live project keeps the daemon's health word", () => {
    expect(projectStateLabel(project())).toBe("healthy");
  });
});

describe("running card (mock B, no chart)", () => {
  const html = renderToStaticMarkup(<ProjectCard project={project()} />);

  test("shows measured totals, not a sparkline", () => {
    expect(html).toContain("4/4");
    expect(html).toContain("45 MB");
    expect(html).toContain("4.4%");
    expect(html).not.toContain("<svg");
    expect(html).not.toContain("polyline");
  });

  test("lists ranked services as chips and a memory share bar", () => {
    expect(html).toContain("runscape");
    expect(html).toContain("python");
    expect(html).toContain("python 26 MB");
    expect(html).toContain("45 MB RSS");
    expect(html).toContain(WARNING.message);
  });
});

describe("idle card", () => {
  const html = renderToStaticMarkup(
    <ProjectCard
      project={project({
        running_service_count: 0,
        memory_bytes: 0,
        cpu_percent: 0,
        health: "stopped",
        ranked_by_cpu: [],
        ranked_by_memory: [],
        dominant_memory: null,
      })}
    />,
  );

  test("does not print zero metrics as if they were observations", () => {
    expect(html).not.toContain("0/4");
    expect(html).not.toContain("0 B");
    expect(html).not.toContain("0.0%");
    expect(html).toContain("not running");
    expect(html).toContain("Last seen with 4 services");
    expect(html).toContain(WARNING.message);
  });
});
