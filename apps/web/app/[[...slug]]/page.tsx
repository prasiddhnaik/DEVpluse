import { DashboardApp } from "@/components/DashboardApp";

/** Optional catch-all so next dev and a hard refresh share one client shell. */
export function generateStaticParams() {
  return [{ slug: [] as string[] }];
}

export default function Page() {
  return <DashboardApp />;
}
