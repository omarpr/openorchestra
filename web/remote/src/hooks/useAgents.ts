import { useCallback, useEffect, useRef, useState } from "react";
import { type Agent, ApiError, UnauthorizedError } from "@/lib/api";
import { closedReason, parseData, streamEvents } from "@/lib/sse";
import { clearToken, getToken } from "@/lib/token";

const RANK: Record<Agent["status"], number> = { waiting: 0, running: 1, idle: 2, unknown: 2, stopped: 3 };

/** A shell tab (no agent kind) has no work status of its own: only agents run or wait. */
export function normalizeAgent(a: Agent): Agent {
  return a.agent_kind === null && a.status !== "stopped" ? { ...a, status: "idle", status_since: null, tool: null } : a;
}

export function sortAgents(list: Agent[]): Agent[] {
  return [...list].sort(
    (a, b) => RANK[a.status] - RANK[b.status] || a.title.localeCompare(b.title, undefined, { sensitivity: "base" }),
  );
}

import { backoffMs } from "@/lib/backoff";

export { backoffMs };

export interface AgentsState {
  agents: Agent[];
  connected: boolean;
  /** At least one `agents` snapshot has arrived on this connection lifetime. */
  loaded: boolean;
  /** Token was rejected (401 or `closed: revoked`); token already cleared. */
  unauthorized: boolean;
  /** Server said remote control is off (`closed: disabled`). */
  disabled: boolean;
  /** The Mac refuses this network (403 `untrusted_network`). */
  untrusted: boolean;
}

/** Live agents over SSE with 1s/2s/4s…15s backoff and instant reconnect on becoming visible. */
export function useAgents(): AgentsState & { retry: () => void } {
  const retryRef = useRef<() => void>(() => {});
  const [state, setState] = useState<AgentsState>({
    agents: [],
    connected: false,
    loaded: false,
    unauthorized: false,
    disabled: false,
    untrusted: false,
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
      setState({ agents: [], connected: false, loaded: false, unauthorized: true, disabled: false, untrusted: false });
    };

    const run = async () => {
      ctrl?.abort();
      clearTimeout(timer);
      const mine = new AbortController();
      ctrl = mine;
      let reason: string | null = null;
      let untrusted = false;
      try {
        await streamEvents(
          "/api/events",
          token,
          (e) => {
            if (mine.signal.aborted) return;
            if (e.event === "closed") reason = closedReason(e.data);
            if (e.event !== "agents") return;
            const list = parseData<Agent[]>(e.data);
            if (!list) return;
            attempt = 0;
            const agents = sortAgents(list.map(normalizeAgent));
            setState((s) => ({ ...s, agents, connected: true, loaded: true, disabled: false, untrusted: false }));
          },
          mine.signal,
        );
      } catch (err) {
        if (mine.signal.aborted || stopped) return;
        if (err instanceof UnauthorizedError) return fail();
        untrusted = err instanceof ApiError && err.code === "untrusted_network";
      }
      if (mine.signal.aborted || stopped) return;
      if (reason === "revoked") return fail();
      setState((s) => ({
        ...s,
        connected: false,
        untrusted,
        disabled: reason === "disabled" ? true : s.disabled,
      }));
      timer = setTimeout(run, backoffMs(attempt++));
    };

    const onVisible = () => {
      if (document.visibilityState === "visible" && !stopped) {
        attempt = 0;
        void run();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    retryRef.current = () => {
      if (stopped) return;
      attempt = 0;
      void run();
    };
    void run();
    return () => {
      stopped = true;
      ctrl?.abort();
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  const retry = useCallback(() => retryRef.current(), []);
  return { ...state, retry };
}
