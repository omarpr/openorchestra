import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";
import { useLeaveUnshared } from "./useLeaveUnshared";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

const a: Agent = {
  id: "a1",
  title: "Fix login",
  agent_kind: null,
  project: "p",
  worktree: "w",
  branch: null,
  has_conversation: false,
  status: "idle",
  status_since: null,
  tool: null,
};

beforeEach(() => {
  toast.mockReset();
  window.location.hash = "#/agent/a1";
});

describe("useLeaveUnshared", () => {
  it("tells the user and goes back to the list when the open tab disappears", () => {
    const { rerender } = renderHook(({ agent }) => useLeaveUnshared("a1", agent, true), {
      initialProps: { agent: a as Agent | undefined },
    });
    rerender({ agent: undefined });
    expect(toast).toHaveBeenCalledWith("“Fix login” is no longer shared", expect.anything());
    expect(window.location.hash).toBe("#/");
  });

  it("leaves a link to an id that was never listed alone", () => {
    renderHook(() => useLeaveUnshared("a1", undefined, true));
    expect(toast).not.toHaveBeenCalled();
    expect(window.location.hash).toBe("#/agent/a1");
  });
});
