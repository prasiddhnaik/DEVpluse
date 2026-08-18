/** Geometry for the service resource chart. Pure functions so tests can pin them. */

/** Map a pointer x in chart units onto the nearest retained sample. */
export function sparklineIndexAt(x: number, width: number, count: number): number {
  if (count <= 1 || width <= 0) return 0;
  const t = Math.min(1, Math.max(0, x / width));
  return Math.round(t * (count - 1));
}

export function sparklineX(index: number, count: number, width: number): number {
  if (count <= 1) return 0;
  return (index / (count - 1)) * width;
}

export function sparklineY(value: number, max: number, height: number): number {
  const usable = Math.max(1, height - 8);
  return height - (value / Math.max(max, 1)) * usable - 4;
}

/** SVG path for a series. Empty input yields an empty string. */
export function sparklinePath(
  values: number[],
  max: number,
  width: number,
  height: number,
): string {
  if (values.length === 0) return "";
  return values
    .map((value, index) => {
      const x = sparklineX(index, values.length, width);
      const y = sparklineY(value, max, height);
      return `${index === 0 ? "M" : "L"} ${x.toFixed(1)} ${y.toFixed(1)}`;
    })
    .join(" ");
}
