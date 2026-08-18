"use client";

/**
 * One measured series. Geometry comes from the daemon's samples; this only
 * draws them. No interpolation, no smoothing — a gap looks like a gap.
 *
 * Sized in CSS (`w-full`, no pixel `width` attribute) so a CSS grid cell with
 * `min-w-0` is not blown out by SVG `min-width: auto`.
 */

import { useEffect, useRef, useState, type PointerEvent } from "react";

import { clock } from "@/lib/format";
import { sparklineIndexAt, sparklinePath, sparklineX, sparklineY } from "@/lib/sparkline";

export interface SeriesPoint {
  at: string;
  value: number;
}

const CHART_HEIGHT = 140;

export function SeriesChart({
  points,
  format,
  strokeClass,
  fillClass,
  label,
  caption,
  empty = "Not enough samples yet.",
}: {
  points: SeriesPoint[];
  format: (value: number) => string;
  strokeClass: string;
  fillClass: string;
  label: string;
  caption?: string;
  empty?: string;
}) {
  if (points.length < 2) {
    return <p className="px-3 py-4 text-sm text-zinc-500">{empty}</p>;
  }
  return (
    <SeriesChartPlot
      points={points}
      format={format}
      strokeClass={strokeClass}
      fillClass={fillClass}
      label={label}
      caption={caption}
    />
  );
}

function SeriesChartPlot({
  points,
  format,
  strokeClass,
  fillClass,
  label,
  caption,
}: {
  points: SeriesPoint[];
  format: (value: number) => string;
  strokeClass: string;
  fillClass: string;
  label: string;
  caption?: string;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [pointerX, setPointerX] = useState<number | null>(null);

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const update = () => {
      const next = Math.floor(el.getBoundingClientRect().width);
      if (next > 0) {
        setWidth(next);
      }
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const height = CHART_HEIGHT;
  const plotWidth = Math.max(width, 1);
  const values = points.map((point) => point.value);
  const max = Math.max(...values, 1);
  const hover =
    pointerX === null ? null : sparklineIndexAt(pointerX, plotWidth, points.length);
  const hovered = hover === null ? null : points[hover];
  const last = points[points.length - 1];
  const hairX = hover === null ? 0 : sparklineX(hover, points.length, plotWidth);
  const d = sparklinePath(values, max, plotWidth, height);

  function move(event: PointerEvent<SVGSVGElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width === 0) return;
    setPointerX(((event.clientX - rect.left) / rect.width) * plotWidth);
  }

  return (
    <div className="min-w-0 px-3 py-2">
      <div ref={wrapRef} className="relative min-w-0 w-full">
        {width === 0 ? (
          <div style={{ height }} aria-hidden />
        ) : (
          <svg
            viewBox={`0 0 ${plotWidth} ${height}`}
            className="block w-full cursor-crosshair"
            style={{ height }}
            preserveAspectRatio="none"
            role="img"
            aria-label={
              hovered
                ? `At ${clock(hovered.at)}, ${label} ${format(hovered.value)}`
                : `${label} over time`
            }
            onPointerMove={move}
            onPointerLeave={() => setPointerX(null)}
          >
            <path
              d={d}
              fill="none"
              strokeWidth={1.5}
              vectorEffect="non-scaling-stroke"
              className={strokeClass}
            />
            {hovered && hover !== null && (
              <g pointerEvents="none">
                <line
                  x1={hairX}
                  y1={0}
                  x2={hairX}
                  y2={height}
                  className="stroke-white/25"
                  strokeWidth={1}
                  vectorEffect="non-scaling-stroke"
                />
                <circle
                  cx={hairX}
                  cy={sparklineY(hovered.value, max, height)}
                  r={3.5}
                  className={fillClass}
                />
              </g>
            )}
          </svg>
        )}
        {hovered && hover !== null && (
          <div
            className="pointer-events-none absolute top-2 z-10 rounded-md border border-line bg-surface px-2 py-1 text-xs tabular-nums shadow-lg"
            style={{
              left: hairX,
              transform:
                hover > (points.length - 1) / 2
                  ? "translateX(calc(-100% - 8px))"
                  : "translateX(8px)",
            }}
          >
            <p className="text-zinc-400">{clock(hovered.at)}</p>
            <p>
              <span className={strokeClass.replace("stroke-", "text-")}>{label}</span>{" "}
              {format(hovered.value)}
            </p>
          </div>
        )}
      </div>
      <p className="pt-1 text-xs text-zinc-500">
        {hovered ? (
          <>
            {format(hovered.value)} · {clock(hovered.at)}
          </>
        ) : (
          <>
            peak {format(max)}
            {caption ? ` · ${caption}` : ""} · {points.length} samples
            {last ? ` to ${clock(last.at)}` : ""}
          </>
        )}
      </p>
    </div>
  );
}
