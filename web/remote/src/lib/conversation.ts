import { useEffect, useState } from "react";
import { UnauthorizedError } from "@/lib/api";
import { backoffMs } from "@/hooks/useAgents";
import { streamEvents } from "@/lib/sse";
import { clearToken, getToken } from "@/lib/token";

export const WINDOW = 200;

/** Mirrors the server's conversation `Entry`. */
export interface ConvEntry {
  id: string;
  kind: "user" | "assistant" | "thinking" | "tool" | "notice";
  text?: string;
  tool?: { id: string; name: string; summary: string; status: "pending" | "ok" | "error" };
  duration_ms?: number;
  at?: string;
}

/** replace → incoming; otherwise upsert by id (known id replaced in place, unknown appended). Last 200 kept. */
export function mergeEntries(list: ConvEntry[], incoming: ConvEntry[], replace: boolean): ConvEntry[] {
  let out: ConvEntry[];
  if (replace) {
    out = incoming;
  } else {
    out = [...list];
    const idx = new Map(out.map((e, i) => [e.id, i]));
    for (const e of incoming) {
      const i = idx.get(e.id);
      if (i === undefined) {
        idx.set(e.id, out.length);
        out.push(e);
      } else {
        out[i] = e;
      }
    }
  }
  return out.length > WINDOW ? out.slice(out.length - WINDOW) : out;
}

export interface ConversationState {
  entries: ConvEntry[];
  connected: boolean;
  closed?: string;
}

/** Live conversation over SSE with 1s…15s backoff; `closed: revoked` clears the token like the agent list. */
export function useConversation(id: string): ConversationState {
  const [state, setState] = useState<ConversationState>({ entries: [], connected: false });

  useEffect(() => {
    const token = getToken();
    if (!token) return;
    let stopped = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ctrl: AbortController | undefined;
    setState({ entries: [], connected: false });

    const finish = (reason: string) => {
      stopped = true;
      ctrl?.abort();
      clearTimeout(timer);
      if (reason === "revoked") clearToken();
      setState((s) => ({ ...s, connected: false, closed: reason }));
    };

    const run = async () => {
      ctrl?.abort();
      clearTimeout(timer);
      const mine = new AbortController();
      ctrl = mine;
      try {
        await streamEvents(
          `/api/agents/${encodeURIComponent(id)}/conversation`,
          token,
          (e) => {
            if (mine.signal.aborted) return;
            if (e.event === "entries") {
              try {
                const d = JSON.parse(e.data) as { replace?: boolean; entries?: ConvEntry[] };
                attempt = 0;
                setState((s) => ({
                  ...s,
                  connected: true,
                  entries: mergeEntries(s.entries, d.entries ?? [], !!d.replace),
                }));
              } catch {
                /* malformed frame */
              }
            } else if (e.event === "closed") {
              let reason = "";
              try {
                reason = (JSON.parse(e.data) as { reason?: string }).reason ?? "";
              } catch {
                reason = e.data;
              }
              finish(reason || "closed");
            }
          },
          mine.signal,
        );
      } catch (err) {
        if (mine.signal.aborted || stopped) return;
        if (err instanceof UnauthorizedError) return finish("revoked");
        // 404: no conversation for this agent (view-local, unlike a real `unmarked`); retrying cannot help.
        if (err instanceof Error && err.message === "HTTP 404") return finish("no_conversation");
      }
      if (mine.signal.aborted || stopped) return;
      setState((s) => ({ ...s, connected: false }));
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
  }, [id]);

  return state;
}
