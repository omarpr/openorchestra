import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";
import { UnauthorizedError } from "@/lib/api";

const clearToken = vi.fn();
vi.mock("@/lib/token", () => ({
  getToken: () => "tok",
  clearToken: () => clearToken(),
}));
const streamEvents = vi.fn();
vi.mock("@/lib/sse", async (orig) => ({
  ...(await orig<typeof import("@/lib/sse")>()),
  streamEvents: (...a: unknown[]) => streamEvents(...a),
}));

import { sortAgents, useAgents } from "./useAgents";

const mk = (id: string, status: Agent["status"], title = id): Agent => ({
  id,
  title,
  agent_kind: "claude",
  project: "p",
  worktree: "w", branch: null, has_conversation: false,
  status,
  status_since: null,
  tool: null,
});

describe("sortAgents", () => {
  it("orders waiting, running, idle/unknown, stopped, then by title", () => {
    const out = sortAgents([
      mk("1", "stopped", "a"),
      mk("2", "idle", "b"),
      mk("3", "running", "z"),
      mk("4", "waiting", "y"),
      mk("5", "unknown", "a"),
      mk("6", "running", "c"),
    ]);
    expect(out.map((a) => a.id)).toEqual(["4", "6", "3", "5", "2", "1"]);
  });
});

describe("useAgents", () => {
  beforeEach(() => {
    clearToken.mockReset();
    streamEvents.mockReset();
  });

  it("delivers sorted agents and marks connected", async () => {
    streamEvents.mockImplementation(async (_u, _t, onEvent, signal: AbortSignal) => {
      onEvent({ event: "agents", data: JSON.stringify([mk("1", "stopped"), mk("2", "waiting")]) });
      await new Promise((r) => signal.addEventListener("abort", r));
    });
    const { result } = renderHook(() => useAgents());
    await waitFor(() => expect(result.current.agents.length).toBe(2));
    expect(result.current.agents.map((a) => a.id)).toEqual(["2", "1"]);
    expect(result.current.connected).toBe(true);
  });

  it("a 401 clears the token and sets unauthorized", async () => {
    streamEvents.mockRejectedValue(new UnauthorizedError());
    const { result } = renderHook(() => useAgents());
    await waitFor(() => expect(result.current.unauthorized).toBe(true));
    expect(clearToken).toHaveBeenCalled();
  });

  it("closed:revoked is treated like a 401", async () => {
    streamEvents.mockImplementation(async (_u, _t, onEvent) => {
      onEvent({ event: "closed", data: JSON.stringify({ reason: "revoked" }) });
    });
    const { result } = renderHook(() => useAgents());
    await waitFor(() => expect(result.current.unauthorized).toBe(true));
    expect(clearToken).toHaveBeenCalled();
  });

  it("closed:disabled sets disabled and reconnects with backoff", async () => {
    vi.useFakeTimers();
    try {
      streamEvents.mockImplementation(async (_u, _t, onEvent) => {
        onEvent({ event: "closed", data: JSON.stringify({ reason: "disabled" }) });
      });
      const { result } = renderHook(() => useAgents());
      await act(async () => {});
      expect(result.current.disabled).toBe(true);
      expect(result.current.connected).toBe(false);
      expect(streamEvents).toHaveBeenCalledTimes(1);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1000);
      });
      expect(streamEvents).toHaveBeenCalledTimes(2);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1999);
      });
      expect(streamEvents).toHaveBeenCalledTimes(2);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(streamEvents).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });
});
