import { describe, expect, test } from "bun:test";

import { sparklineIndexAt, sparklinePath, sparklineX, sparklineY } from "./sparkline";

describe("sparklineIndexAt", () => {
  test("pins the left edge to the first sample", () => {
    expect(sparklineIndexAt(0, 480, 300)).toBe(0);
    expect(sparklineIndexAt(-40, 480, 300)).toBe(0);
  });

  test("pins the right edge to the last sample", () => {
    expect(sparklineIndexAt(480, 480, 300)).toBe(299);
    expect(sparklineIndexAt(999, 480, 300)).toBe(299);
  });

  test("picks the nearest sample in the middle", () => {
    expect(sparklineIndexAt(240, 480, 5)).toBe(2);
  });
});

describe("sparkline coordinates", () => {
  test("x spreads samples across the drawn width", () => {
    expect(sparklineX(0, 5, 400)).toBe(0);
    expect(sparklineX(4, 5, 400)).toBe(400);
  });

  test("y puts the max at the top padding", () => {
    expect(sparklineY(10, 10, 100)).toBe(4);
    expect(sparklineY(0, 10, 100)).toBe(96);
  });

  test("path walks left to right without interpolating", () => {
    expect(sparklinePath([0, 10], 10, 400, 100)).toBe("M 0.0 96.0 L 400.0 4.0");
    expect(sparklinePath([], 10, 400, 100)).toBe("");
  });
});
