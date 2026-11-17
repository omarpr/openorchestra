import { useCallback, useEffect, useRef, useState } from "react";
import { type Closure, isRemoteWide, isRetryable } from "@/lib/agents";
import type { View } from "./useViewChoice";

/**
 * Why the open view's stream ended, if it did, and how to start a fresh one. `attempt` keys the
 * view so a retry remounts it with a new connection. When the Mac turns remote control back on
 * after closing the view, it reconnects by itself.
 */
export function useClosures(view: View, remoteUp: boolean) {
  const [closedBy, setClosedBy] = useState<Partial<Record<View, Closure>>>({});
  const [connected, setConnected] = useState(true);
  const [attempt, setAttempt] = useState(0);

  // A view that (re)mounts starts fresh, unless its closure was remote-wide.
  useEffect(() => {
    setClosedBy((c) => {
      const r = c[view];
      if (!r || isRemoteWide(r)) return c;
      const { [view]: _gone, ...rest } = c;
      return rest;
    });
  }, [view]);

  const other = view === "terminal" ? closedBy.conversation : closedBy.terminal;
  const closed = closedBy[view] ?? (other && isRemoteWide(other) ? other : null);

  const report = useCallback((r: Closure) => setClosedBy((c) => ({ ...c, [view]: r })), [view]);
  const retry = useCallback(() => {
    setClosedBy((c) => Object.fromEntries(Object.entries(c).filter(([, r]) => r && !isRetryable(r))));
    setConnected(true);
    setAttempt((n) => n + 1);
  }, []);

  const wasUp = useRef(remoteUp);
  useEffect(() => {
    const rose = remoteUp && !wasUp.current;
    wasUp.current = remoteUp;
    if (rose && closed === "disabled") retry();
  }, [remoteUp, closed, retry]);

  return { closed, connected, setConnected, report, retry, attempt };
}
