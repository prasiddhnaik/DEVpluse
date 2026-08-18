import { describe, expect, test } from "bun:test";

import { parseRoute } from "./route";

describe("parseRoute", () => {
  test("the home path is the project overview", () => {
    expect(parseRoute("/")).toEqual({ view: "overview" });
    expect(parseRoute("")).toEqual({ view: "overview" });
  });

  test("project and service ids are taken from the path", () => {
    expect(parseRoute("/projects/prj_abc")).toEqual({
      view: "project",
      id: "prj_abc",
    });
    expect(parseRoute("/services/svc_1/")).toEqual({
      view: "service",
      id: "svc_1",
    });
  });

  test("unknown paths fall back to the overview rather than inventing a view", () => {
    expect(parseRoute("/not-a-page")).toEqual({ view: "overview" });
    expect(parseRoute("/projects")).toEqual({ view: "overview" });
  });
});
