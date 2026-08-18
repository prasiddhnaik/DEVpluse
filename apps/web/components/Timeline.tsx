"use client";

/**
 * Project timeline (task T4.6) and, on click, an event's context (T7.4).
 *
 * The context view is the "what changed?" answer: it shows what happened around
 * an event, labelled with *why* it is related — and never labelled as a cause
 * (`DECISIONS.md` D008).
 */

import { useEffect, useState } from "react";

import { api } from "@/lib/api";
import { clock, describeEvent, eventTone, offset } from "@/lib/format";
import type { RunscapeEvent, EventContext, Relation, Service } from "@/lib/types";

const RELATION_LABEL: Record<Relation, string> = {
  same_service: "same service",
  same_project: "same project",
  preceding_file_change: "you saved this just before",
  temporal: "around the same time",
};

export function Timeline({
  events,
  services,
}: {
  events: RunscapeEvent[];
  services: Service[];
}) {
  const [openId, setOpenId] = useState<string | null>(null);
  const nameOf = (id: string) =>
    services.find((service) => service.id === id)?.name ?? id.slice(0, 12);

  if (events.length === 0) {
    return (
      <p className="px-3 py-6 text-sm text-zinc-500">
        Nothing has happened yet. Start or stop a service and it will appear here.
      </p>
    );
  }

  return (
    <ol className="flex flex-col">
      {events.map((event) => (
        <li key={event.id} className="border-b border-line last:border-b-0">
          <button
            type="button"
            onClick={() => setOpenId(openId === event.id ? null : event.id)}
            className="flex w-full items-baseline gap-3 px-1 py-2 text-left text-sm transition hover:bg-surface-raised"
            aria-expanded={openId === event.id}
          >
            <span className="font-mono text-xs text-zinc-500 tabular-nums">
              {clock(event.at)}
            </span>
            <span className={`mt-1.5 size-2 shrink-0 rounded-full ${eventTone(event)}`} />
            <span className="flex-1">{describeEvent(event, nameOf)}</span>
            <span className="text-xs text-zinc-400">
              {openId === event.id ? "hide" : "context"}
            </span>
          </button>

          {openId === event.id && (
            // Keyed so switching events starts from a clean panel rather than
            // showing the previous event's context while the next one loads.
            <ContextPanel key={event.id} eventId={event.id} nameOf={nameOf} />
          )}
        </li>
      ))}
    </ol>
  );
}

function ContextPanel({
  eventId,
  nameOf,
}: {
  eventId: string;
  nameOf: (id: string) => string;
}) {
  const [context, setContext] = useState<EventContext | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();

    api
      .eventContext(eventId, undefined, controller.signal)
      .then(setContext)
      .catch((cause: unknown) => {
        if (controller.signal.aborted) return;
        setError(cause instanceof Error ? cause.message : "context unavailable");
      });

    return () => controller.abort();
  }, [eventId]);

  if (error) {
    return <p className="px-8 pb-3 text-xs text-rose-600">{error}</p>;
  }
  if (!context) {
    return <p className="px-8 pb-3 text-xs text-zinc-500">loading context…</p>;
  }

  const related = [...context.before, ...context.after];
  if (related.length === 0) {
    return (
      <p className="px-8 pb-3 text-xs text-zinc-500">
        Nothing else happened within {context.window_ms / 1000}s.
      </p>
    );
  }

  return (
    <div className="px-8 pb-3">
      <p className="pb-1 text-xs text-zinc-500">
        Within {context.window_ms / 1000}s — ordering only, not causation.
      </p>
      <ul className="flex flex-col gap-1">
        {related
          .sort((a, b) => a.offset_ms - b.offset_ms)
          .map((item) => (
            <li key={item.id} className="flex items-baseline gap-3 text-xs">
              <span className="w-14 shrink-0 text-right font-mono text-zinc-500 tabular-nums">
                {offset(item.offset_ms)}
              </span>
              <span className="flex-1">{describeEvent(item, nameOf)}</span>
              <span className="text-zinc-400">{RELATION_LABEL[item.relation]}</span>
            </li>
          ))}
      </ul>
    </div>
  );
}
