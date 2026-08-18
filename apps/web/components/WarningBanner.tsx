import { ago, severityStyle } from "@/lib/format";
import type { Warning } from "@/lib/types";

/**
 * Warnings come from the daemon's deterministic rules (`TASKS.md` T7.3), so
 * each one names the rule that fired. A developer can then decide whether they
 * agree with it.
 */
export function WarningBanner({ warnings }: { warnings: Warning[] }) {
  if (warnings.length === 0) return null;

  return (
    <ul className="flex flex-col gap-2">
      {warnings.map((warning) => (
        <li
          key={warning.id}
          className={`flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-lg border px-3 py-2 text-sm ${severityStyle[warning.severity]}`}
        >
          <span className="font-medium">{warning.message}</span>
          <span className="font-mono text-xs opacity-70">{warning.rule}</span>
          <span className="ml-auto text-xs opacity-70">
            since {ago(warning.first_seen)}
          </span>
        </li>
      ))}
    </ul>
  );
}
