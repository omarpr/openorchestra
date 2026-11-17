import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Agent } from "@/lib/api";
import { NoSelection } from "./NoSelection";

const agent = (id: string, status: Agent["status"]): Agent => ({
  id,
  title: `Tab ${id}`,
  agent_kind: "claude-code",
  project: "p",
  worktree: "w",
  branch: null,
  has_conversation: false,
  status,
  status_since: null,
  tool: null,
});

describe("NoSelection", () => {
  it("says nothing is shared when the list is empty", () => {
    render(<NoSelection agents={[]} />);
    expect(screen.getByRole("heading", { name: "Nothing shared yet" })).toBeInTheDocument();
  });

  it("counts shared tabs and links straight to one that needs input", () => {
    render(<NoSelection agents={[agent("a", "running"), agent("b", "waiting")]} />);
    expect(screen.getByText(/2 tabs are shared/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Tab b/ })).toHaveAttribute("href", "#/agent/b");
  });
});
