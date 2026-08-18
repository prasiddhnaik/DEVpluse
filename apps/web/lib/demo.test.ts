import { describe, expect, test } from "bun:test";

import { DEMO_SERVICES, DEMO_STATUS, demoServiceDetail } from "./demo";

describe("demo fixtures", () => {
  test("resource history carries every measured field the charts bind to", () => {
    const sample = DEMO_SERVICES[0]?.resource_history?.at(-1);
    expect(sample).toBeDefined();
    expect(sample?.cpu_percent).toBeGreaterThan(0);
    expect(sample?.memory_bytes).toBeGreaterThan(0);
    expect(sample?.virtual_memory_bytes).toBeGreaterThan(0);
    expect(sample?.thread_count).toBeGreaterThan(0);
    expect(sample?.disk_read_bytes).toBeGreaterThanOrEqual(0);
    expect(sample?.disk_write_bytes).toBeGreaterThanOrEqual(0);
    expect(sample?.connection_count).toBeGreaterThanOrEqual(0);
  });

  test("host history is long enough to draw load and process-count charts", () => {
    expect(DEMO_STATUS.host_history?.length).toBeGreaterThanOrEqual(2);
    expect(DEMO_STATUS.host?.process_count).toBeGreaterThan(0);
    expect(DEMO_STATUS.host?.load_avg_1).toBeGreaterThan(0);
  });

  test("service detail splits observed edges without inventing new ones", () => {
    const web = demoServiceDetail("svc_demo_web");
    expect(web?.connections.outbound).toHaveLength(1);
    expect(web?.connections.inbound).toHaveLength(0);
    expect(demoServiceDetail("svc_missing")).toBeNull();
  });
});
