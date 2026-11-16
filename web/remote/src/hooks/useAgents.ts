import { useEffect, useState } from "react";
import { UnauthorizedError, type Agent } from "@/lib/api";
import { streamEvents } from "@/lib/sse";
import { clearToken, getToken } from "@/lib/token";

const RANK: Record<Agent["status"], number> = { waiting: 0, running: 1, idle: 2, unknown: 2, stopped: 3 };

export function sortAgents(list: Agent[]): Agent[] {
  return [...list].sort(
    (a, b) => RANK[a.status] - RANK[b.status] || a.title.localeCompare(b.title, undefined, { sensitivity: "base" }),
  );
}

export const backoffMs = (attempt: number) => Math.min(1000 * 2 ** attempt, 15000);

export interface AgentsState {
  agents: Agent[];
  connected: boolean;
  /** Token was rejected (401 or `closed: revoked`); token already cleared. */
  unauthorized: boolean;
  /** Server said remote control is off (`closed: disabled`). */
  disabled: boolean;
}

/** Live agents over SSE with 1s/2s/4s…15s backoff and instant reconnect on becoming visible. */
export function useAgents(): AgentsState {
  const [state, setState] = useState<AgentsState>({
    agents: [],
    connected: false,
    unauthorized: false,
    disabled: false,
  });

  useEffect(() => {
    const token = getToken();
    if (!token) {
      setState((s) => ({ ...s, unauthorized: true }));
      return;
    }
    let stopped = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ctrl: AbortController | undefined;

    const fail = () => {
      stopped = true;
      ctrl?.abort();
      clearTimeout(timer);
      clearToken();
      setState({ agents: [], connected: false, unauthorized: true, disabled: false });
    };

    const run = async () => {
      ctrl?.abort();
      clearTimeout(timer);
      const mine = new AbortController();
      ctrl = mine;
      let closedReason: string | null = null;
      try {
        await streamEvents(
          "/api/events",
          token,
          (e) => {
            if (mine.signal.aborted) return;
            if (e.event === "agents") {
              try {
                const list = JSON.parse(e.data) as Agent[];
                attempt = 0;
                setState((s) => ({ ...s, agents: sortAgents(list), connected: true, disabled: false }));
              } catch {
                /* malformed frame */
              }
            } else if (e.event === "closed") {
              try {
                closedReason = (JSON.parse(e.data) as { reason?: string }).reason ?? "";
              } catch {
                closedReason = e.data;
              }
            }
          },
          mine.signal,
        );
      } catch (err) {
        if (mine.signal.aborted || stopped) return;
        if (err instanceof UnauthorizedError) return fail();
      }
      if (mine.signal.aborted || stopped) return;
      if (closedReason === "revoked") return fail();
      setState((s) => ({ ...s, connected: false, disabled: closedReason === "disabled" ? true : s.disabled }));
      timer = setTimeout(run, backoffMs(attempt++));
    };

    const onVisible = () => {
      if (document.visibilityState === "visible" && !stopped) {
        attempt = 0;
        void run();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    void run();
    return () => {
      stopped = true;
      ctrl?.abort();
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  return state;
}
