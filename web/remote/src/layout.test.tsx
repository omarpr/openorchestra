import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";

const agents: Agent[] = [
  {
    id: "a1",
    title: "Fix login bug",
    agent_kind: "claude",
    project: "proj",
    worktree: "main",
    branch: null,
    has_conversation: false,
    status: "running",
    status_since: null,
    tool: "Edit",
  },
];
vi.mock("@/hooks/useAgents", () => ({
  useAgents: () => ({ agents, connected: true, unauthorized: false, disabled: false }),
}));

import App from "./App";

function mockMedia(matches: boolean) {
  window.matchMedia = ((q: string) => ({
    matches,
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    onchange: null,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe("layout", () => {
  beforeEach(() => {
    localStorage.setItem("oo.token", "tok");
    window.location.hash = "";
  });

  it("tablet: list and the detail placeholder both render", () => {
    mockMedia(true);
    render(<App />);
    expect(screen.getByText("Fix login bug")).toBeInTheDocument();
    expect(screen.getByText("Pick a tab")).toBeInTheDocument();
  });

  it("phone: only the list renders", () => {
    mockMedia(false);
    render(<App />);
    expect(screen.getByText("Fix login bug")).toBeInTheDocument();
    expect(screen.queryByText("Pick a tab")).not.toBeInTheDocument();
  });
});
