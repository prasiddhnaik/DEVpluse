"use client";

import { NavLink } from "@/components/NavLink";
import { isDemoMode } from "@/lib/demo";
import { useDaemon } from "@/lib/daemon";
import { ConnectionBadge } from "./ConnectionBadge";

/**
 * Frame around every page: the product name, the daemon's connection state
 * (task T4.2), and what it can see.
 */
export function AppShell({ children }: { children: React.ReactNode }) {
  const { status } = useDaemon();
  const demo = isDemoMode();

  return (
    <div className="mx-auto flex min-h-full max-w-7xl flex-col gap-6 px-6 py-6">
      <header className="flex flex-wrap items-center justify-between gap-4 border-b border-line pb-4">
        <div className="flex items-baseline gap-3">
          <NavLink href="/" className="text-lg font-semibold tracking-tight">
            Runscape
          </NavLink>
          <span className="text-xs text-zinc-500">
            {status ? `v${status.version} · ${status.platform.os}` : "local"}
          </span>
        </div>

        <div className="flex items-center gap-4">
          {status && (
            <dl className="hidden items-center gap-4 text-xs text-zinc-500 sm:flex">
              <Counter label="projects" value={status.counts.projects} />
              <Counter label="services" value={status.counts.services} />
              <Counter label="edges" value={status.counts.connections} />
              <Counter
                label="docker"
                value={status.docker.available ? "on" : "off"}
                title={status.docker.reason}
              />
            </dl>
          )}
          <ConnectionBadge />
        </div>
      </header>

      {demo && (
        <p
          role="status"
          className="rounded-lg border border-amber-800 bg-amber-950 px-3 py-2 text-sm text-amber-200"
        >
          Demo — sample data, not your machine
        </p>
      )}

      <main className="min-w-0 flex-1">{children}</main>

      <footer className="border-t border-line pt-3 text-xs text-zinc-500">
        {demo
          ? "Demo — sample data, not your machine. The live observer never binds a public address."
          : "Everything here is observed on this machine and stays on it. Nothing is uploaded."}
      </footer>
    </div>
  );
}

function Counter({
  label,
  value,
  title,
}: {
  label: string;
  value: number | string;
  title?: string;
}) {
  return (
    <div className="flex items-baseline gap-1" title={title}>
      <dt className="sr-only">{label}</dt>
      <dd className="font-medium text-ink tabular-nums">
        {value}
      </dd>
      <span aria-hidden>{label}</span>
    </div>
  );
}
