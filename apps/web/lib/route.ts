/**
 * Client routes for the dashboard. The daemon serves one HTML shell and these
 * paths; Next file routes would not survive a static export of unknown ids.
 */

export type Route =
  | { view: "overview" }
  | { view: "project"; id: string }
  | { view: "service"; id: string };

export function parseRoute(pathname: string): Route {
  const parts = pathname.split("/").filter((part) => part.length > 0);
  if (parts[0] === "projects" && parts[1]) {
    return { view: "project", id: decodeURIComponent(parts[1]) };
  }
  if (parts[0] === "services" && parts[1]) {
    return { view: "service", id: decodeURIComponent(parts[1]) };
  }
  return { view: "overview" };
}

export function navigate(href: string): void {
  window.history.pushState(null, "", href);
  window.dispatchEvent(new PopStateEvent("popstate"));
}
