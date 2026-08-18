import { DashboardApp } from "@/components/DashboardApp";
import { DEMO_STATIC_SLUGS } from "@/lib/demo";

/** Optional catch-all so next dev and a hard refresh share one client shell. */
export function generateStaticParams() {
  return DEMO_STATIC_SLUGS.map((slug) => ({ slug }));
}

export default function Page() {
  return <DashboardApp />;
}
