"use client";

import { localPulseWorker, useDaemon } from "@/lib/daemon";

/**
 * Local pulse worker connection state (task T4.2). Internally this is the
 * daemon (`runscape serve`); the UI never uses that word.
 *
 * A dashboard that silently shows stale data is worse than one that admits it
 * is disconnected, so this is always visible and never optimistic.
 */
export function ConnectionBadge() {
  const { connection, reconnects, lastFrameAt, resnapshot } = useDaemon();

  const style = {
    connecting: { dot: "bg-accent animate-pulse", label: "connecting" },
    connected: { dot: "bg-emerald-500", label: "connected" },
    reconnecting: { dot: "bg-amber-500 animate-pulse", label: "reconnecting" },
    disconnected: { dot: "bg-rose-500", label: `${localPulseWorker()} disconnected` },
  }[connection];

  return (
    <div className="flex items-center gap-2">
      <span
        role="status"
        className="flex items-center gap-2 rounded-full border border-line bg-surface-raised px-3 py-1 text-xs"
        title={lastFrameAt ? `last update ${lastFrameAt}` : undefined}
      >
        <span className={`size-2 rounded-full ${style.dot}`} aria-hidden />
        <span>{style.label}</span>
        {reconnects > 1 && (
          <span className="text-zinc-500">· {reconnects} reconnects</span>
        )}
      </span>

      {connection === "connected" && (
        <button
          type="button"
          onClick={resnapshot}
          className="rounded-md border border-line px-2 py-1 text-xs text-muted transition hover:bg-zinc-800"
          title={`Ask the ${localPulseWorker()} for a fresh snapshot`}
        >
          refresh
        </button>
      )}
    </div>
  );
}
