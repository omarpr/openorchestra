import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Agent } from "@/lib/api";
import { AgentCard } from "./AgentCard";

const base: Agent = {
  id: "a1",
  title: "Fix login bug",
  agent_kind: "claude-code",
  project: "proj",
  worktree: "main",
  branch: null,
  has_conversation: false,
  status: "running",
  status_since: null,
  tool: "Edit",
};

describe("AgentCard", () => {
  it("shows the agent icon instead of the kind text", () => {
    const { container } = render(<AgentCard agent={base} />);
    expect(container.querySelector('[data-agent="claude-code"]')).toBeInTheDocument();
    expect(screen.queryByText("claude-code")).not.toBeInTheDocument();
    expect(screen.getByText("proj · main")).toBeInTheDocument();
    expect(screen.getByText("Edit")).toBeInTheDocument();
  });
  it("uses a monogram for unknown kinds", () => {
    const { container } = render(<AgentCard agent={{ ...base, agent_kind: "my-agent" }} />);
    const el = container.querySelector('[data-agent="my-agent"]');
    expect(el).toHaveTextContent("◇");
  });
  it("maps cursor-agent to the cursor icon", () => {
    const { container } = render(<AgentCard agent={{ ...base, agent_kind: "cursor-agent" }} />);
    expect(container.querySelector('[data-agent="cursor-agent"]')).not.toHaveTextContent("◇");
  });
});
