import path from "node:path";
import { fileURLToPath } from "node:url";

import type { NextConfig } from "next";

const appDir = path.dirname(fileURLToPath(import.meta.url));

const config: NextConfig = {
  // The daemon embeds this export and serves it on loopback. There is no
  // Next server in `runscape serve`.
  output: "export",
  images: { unoptimized: true },
  // The dashboard is a client of the local daemon and holds no server state of
  // its own (AGENTS.md rule 8), so there is nothing here to configure yet.
  reactStrictMode: true,
  // `next dev` otherwise writes AGENTS.md / CLAUDE.md into this app on every
  // start. The repo already has a root AGENTS.md; those generated files are
  // Next's training-data warning, not product docs.
  agentRules: false,
  // Pin the Turbopack root to this app. Walking up from here finds a
  // `pnpm-lock.yaml` in the home directory, which Next then ignores — and the
  // CSS pipeline resolves against the wrong tree.
  turbopack: {
    root: appDir,
  },
};

export default config;
