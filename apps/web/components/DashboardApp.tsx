"use client";

import { useEffect, useState } from "react";

import { Overview } from "@/components/views/Overview";
import { ProjectView } from "@/components/views/ProjectView";
import { ServiceView } from "@/components/views/ServiceView";
import { parseRoute } from "@/lib/route";

/**
 * Path-based views inside one HTML shell. The daemon serves this shell for
 * every dashboard URL so `cargo install` does not need a second Next process.
 */
export function DashboardApp() {
  const [path, setPath] = useState<string | null>(null);

  useEffect(() => {
    const sync = () => setPath(window.location.pathname);
    sync();
    window.addEventListener("popstate", sync);
    return () => window.removeEventListener("popstate", sync);
  }, []);

  if (path === null) {
    return <p className="text-sm text-zinc-500">Loading…</p>;
  }

  const route = parseRoute(path);
  switch (route.view) {
    case "overview":
      return <Overview />;
    case "project":
      return <ProjectView id={route.id} />;
    case "service":
      return <ServiceView id={route.id} />;
    default: {
      const _exhaustive: never = route;
      return _exhaustive;
    }
  }
}
