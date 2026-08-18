/** The display layer: it must never invent a fact the daemon did not state. */

import { describe, expect, test } from "bun:test";

import { ago, bytes, describeEvent, offset, percent, shortenPath } from "./format";
import type { DevPulseEvent } from "./types";

const nameOf = (id: string) => (id === "svc_a" ? "web" : id);

function event(kind: DevPulseEvent["kind"]): DevPulseEvent {
  return { id: "evt_1", at: "2026-08-18T10:00:00Z", project_id: "prj_1", kind };
}

describe("units", () => {
  test("bytes scale the way a developer reads them", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(2048)).toBe("2.0 KB");
    expect(bytes(40 * 1024 * 1024)).toBe("40 MB");
    expect(bytes(3.5 * 1024 ** 3)).toBe("3.5 GB");
  });

  test("percentages keep one decimal only while it matters", () => {
    expect(percent(0.42)).toBe("0.4%");
    expect(percent(94.6)).toBe("95%");
  });

  test("offsets are signed, because before and after is the point", () => {
    expect(offset(-2000)).toBe("-2.0s");
    expect(offset(5000)).toBe("+5.0s");
    expect(offset(0)).toBe("0.0s");
  });
});

describe("ago", () => {
  const now = Date.parse("2026-08-18T10:00:00Z");

  test("reads in the largest useful unit", () => {
    expect(ago("2026-08-18T09:59:50Z", now)).toBe("10s ago");
    expect(ago("2026-08-18T09:55:00Z", now)).toBe("5m ago");
    expect(ago("2026-08-18T07:00:00Z", now)).toBe("3h ago");
    expect(ago("2026-08-16T10:00:00Z", now)).toBe("2d ago");
  });

  test("an unparseable timestamp is a dash, never a guess", () => {
    expect(ago("not a time", now)).toBe("—");
  });
});

describe("describeEvent", () => {
  test("names the service, and the pid when there is one", () => {
    expect(
      describeEvent(event({ type: "service_started", service_id: "svc_a", pid: 100 }), nameOf),
    ).toBe("web started (pid 100)");
  });

  test("omits the pid for a container, which has none", () => {
    expect(
      describeEvent(event({ type: "service_started", service_id: "svc_a", pid: null }), nameOf),
    ).toBe("web started");
  });

  test("spells out a restart as one pid becoming another", () => {
    expect(
      describeEvent(
        event({ type: "service_restarted", service_id: "svc_a", old_pid: 1, new_pid: 2 }),
        nameOf,
      ),
    ).toBe("web restarted (pid 1 → 2)");
  });

  test("reports a health change in the daemon's own words", () => {
    expect(
      describeEvent(
        event({ type: "health_changed", service_id: "svc_a", from: "healthy", to: "degraded" }),
        nameOf,
      ),
    ).toBe("web went healthy → degraded");
  });

  test("shows a changed file by its tail", () => {
    expect(
      describeEvent(
        event({ type: "file_changed", project_id: "prj_1", path: "/home/dev/app/src/main.rs" }),
        nameOf,
      ),
    ).toBe("file changed: …/app/src/main.rs");
  });
});

describe("shortenPath", () => {
  test("keeps the end, which is the part that identifies it", () => {
    expect(shortenPath("/a/b/c/d/e", 2)).toBe("…/d/e");
    expect(shortenPath("/a/b", 3)).toBe("/a/b");
  });
});
