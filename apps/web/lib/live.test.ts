/**
 * Contract check against a *running* daemon.
 *
 * Skipped when nothing is listening on 7778, so `bun test` stays useful without
 * one. When a daemon is up, this is what proves the dashboard and the daemon
 * still agree — a TypeScript type is a claim, and this is the check.
 *
 * Run it with: `devpulse serve` in one terminal, `bun test` in another.
 */

import { describe, expect, test } from "bun:test";

import { DAEMON_HTTP, DAEMON_WS, api } from "./api";
import type { ServerFrame } from "./types";

const daemonUp = await fetch(`${DAEMON_HTTP}/api/v1/status`)
  .then((response) => response.ok)
  .catch(() => false);

const live = daemonUp ? describe : describe.skip;

live("a running daemon", () => {
  test("reports its platform and its collectors", async () => {
    const status = await api.status();

    expect(status.version).toMatch(/^\d+\.\d+\.\d+$/);
    expect(["macos", "linux", "windows"]).toContain(status.platform.os);
    expect(status.platform.notes.length).toBeGreaterThan(0);
    expect(status.collectors.process.last_run).toBeTruthy();
    expect(typeof status.counts.services).toBe("number");
  });

  test("lists projects with the fields the cards render", async () => {
    const projects = await api.projects();
    expect(Array.isArray(projects)).toBe(true);

    for (const project of projects) {
      expect(project.id).toStartWith("prj_");
      expect(["healthy", "degraded", "stopped", "unknown"]).toContain(project.health);
      expect(project.service_count).toBeGreaterThanOrEqual(project.running_service_count);
    }
  });

  test("every edge carries evidence, as the graph requires", async () => {
    const projects = await api.projects();
    for (const project of projects) {
      const graph = await api.graph(project.id);
      for (const edge of graph.edges) {
        expect(edge.evidence.evidence_type).toBeTruthy();
        expect(edge.evidence.confidence).toBeGreaterThan(0);
        expect(edge.evidence.confidence).toBeLessThanOrEqual(1);
        expect(edge.evidence.first_seen).toBeTruthy();
      }
    }
  });

  test("refuses an unknown project with the contract's error shape", async () => {
    const response = await fetch(`${DAEMON_HTTP}/api/v1/projects/prj_nope`);
    expect(response.status).toBe(404);
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("not_found");
  });

  test("sends exactly one snapshot on connect", async () => {
    const frame = await new Promise<ServerFrame>((resolve, reject) => {
      const socket = new WebSocket(DAEMON_WS);
      const timer = setTimeout(() => {
        socket.close();
        reject(new Error("no frame within 5s"));
      }, 5_000);

      socket.onmessage = (message) => {
        clearTimeout(timer);
        socket.close();
        resolve(JSON.parse(message.data as string) as ServerFrame);
      };
      socket.onerror = () => {
        clearTimeout(timer);
        reject(new Error("socket failed"));
      };
    });

    expect(frame.type).toBe("snapshot");
    if (frame.type !== "snapshot") return;
    expect(frame.status.version).toBeTruthy();
    expect(Array.isArray(frame.services)).toBe(true);
    expect(Array.isArray(frame.warnings)).toBe(true);
  });

  test("secret-looking command arguments arrive already redacted", async () => {
    // The daemon redacts at capture time, so the browser must never see a
    // secret's value — only the marker that says one was there
    // (`AGENTS.md` rule 6).
    const assignment = /(?:api[_-]?key|secret|token|password|passwd|credential)=([^\s]+)/gi;
    const projects = await api.projects();

    for (const project of projects) {
      const detail = await api.project(project.id);
      for (const service of detail.services) {
        for (const instance of service.instances) {
          for (const [match, value] of instance.command
            .join(" ")
            .matchAll(assignment)) {
            expect(value, `unredacted value in: ${match}`).toBe("<redacted>");
          }
        }
      }
    }
  });
});
