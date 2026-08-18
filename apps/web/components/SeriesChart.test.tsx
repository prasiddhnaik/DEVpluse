import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { SeriesChart } from "./SeriesChart";

describe("SeriesChart", () => {
  test("asks for more samples instead of drawing a single point", () => {
    const html = renderToStaticMarkup(
      <SeriesChart
        points={[{ at: "2026-08-18T10:00:00Z", value: 1 }]}
        format={(value) => String(value)}
        strokeClass="stroke-sky-500"
        fillClass="fill-sky-500"
        label="cpu"
      />,
    );
    expect(html).toContain("Not enough samples yet.");
    expect(html).not.toContain("<svg");
  });

  test("keeps the grid from overflowing: no pixel width attribute, min-w-0 wrapper", () => {
    const html = renderToStaticMarkup(
      <SeriesChart
        points={[
          { at: "2026-08-18T10:00:00Z", value: 1 },
          { at: "2026-08-18T10:00:01Z", value: 4 },
        ]}
        format={(value) => `${value}%`}
        strokeClass="stroke-sky-500"
        fillClass="fill-sky-500"
        label="cpu"
      />,
    );
    expect(html).toContain("min-w-0");
    expect(html).not.toMatch(/\swidth="\d+"/);
  });
});
