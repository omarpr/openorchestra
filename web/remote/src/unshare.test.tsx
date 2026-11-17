import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";

// The list the app sees; tests replace it to simulate the Mac's updates.
const state = vi.hoisted(() => ({ agents: [] as Agent[] }));
vi.mock("@/hooks/useAgents", () => ({
  useAgents: () => ({ agents: state.agents, connected: true, disabled: false, loaded: true, retry: () => {} }),
}));
vi.mock("@/components/AgentView", () => ({ AgentView: ({ id }: { id: string }) => <div>viewing {id}</div> }));

import App from "./App";

const tab: Agent = {
  id: "a1",
  title: "Fix login bug",
  agent_kind: "claude-code",
  project: "proj",
  worktree: "main",
  branch: null,
  has_conversation: true,
  status: "running",
  status_since: null,
  tool: null,
};

function tablet() {
  window.matchMedia = ((q: string) => ({
    matches: true,
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    onchange: null,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe("unsharing the open tab", () => {
  beforeEach(() => {
    tablet();
    localStorage.setItem("oo.token", "tok");
    state.agents = [tab];
    window.location.hash = "#/agent/a1";
  });

  it("returns to the unselected state with a notice", async () => {
    const { rerender } = render(<App />);
    expect(screen.getByText("viewing a1")).toBeInTheDocument();

    // The Mac stops sharing it: it leaves the list.
    state.agents = [];
    await act(async () => {
      rerender(<App />);
    });
    // hashchange is async in jsdom: let the router catch up.
    await act(async () => {
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });

    expect(window.location.hash).toBe("#/");
    expect(screen.queryByText("viewing a1")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Nothing shared yet" })).toBeInTheDocument();
    expect(await screen.findByText("“Fix login bug” is no longer shared")).toBeInTheDocument();
  });
});

describe("tablet with nothing shared", () => {
  it("shows only the big empty state: no list column, no list toggle", () => {
    tablet();
    localStorage.setItem("oo.token", "tok");
    state.agents = [];
    window.location.hash = "";
    render(<App />);
    expect(screen.getByRole("heading", { name: "Nothing shared yet" })).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Agents" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /agent list/ })).not.toBeInTheDocument();
  });
});
