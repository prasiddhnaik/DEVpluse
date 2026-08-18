"use client";

import type { MouseEvent, ReactNode } from "react";

import { navigate } from "@/lib/route";

/**
 * In-app link that keeps the WebSocket alive. A full page load would tear
 * down the live view; modifier-clicks still use the browser as usual.
 */
export function NavLink({
  href,
  className,
  children,
  title,
}: {
  href: string;
  className?: string;
  children: ReactNode;
  title?: string;
}) {
  return (
    <a href={href} className={className} title={title} onClick={onNavClick(href)}>
      {children}
    </a>
  );
}

function onNavClick(href: string) {
  return (event: MouseEvent<HTMLAnchorElement>) => {
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.altKey ||
      event.ctrlKey ||
      event.shiftKey
    ) {
      return;
    }
    event.preventDefault();
    navigate(href);
  };
}
