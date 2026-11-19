import { render, screen, within } from "@testing-library/react";
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
  it("uses one shell icon and removes the desktop prompt glyph from the title", () => {
    const { container } = render(<AgentCard agent={{ ...base, title: "❯_ shell", agent_kind: null }} />);
    expect(screen.getByText("shell")).toBeInTheDocument();
    expect(screen.queryByText("❯_ shell")).not.toBeInTheDocument();
    expect(container.querySelector('[data-agent="shell"] svg')).toBeInTheDocument();
  });
  it("puts elapsed time alongside the running badge", () => {
    render(<AgentCard agent={{ ...base, status_since: new Date(Date.now() - 65_000).toISOString() }} />);
    const elapsed = screen.getByText("1m");
    expect(elapsed.tagName).toBe("TIME");
    expect(within(elapsed.parentElement as HTMLElement).getByText("RUNNING")).toBeInTheDocument();
  });
  it.each(["codex", "pi", null])("uses the same centered icon slot for %s", (kind) => {
    const { container } = render(<AgentCard agent={{ ...base, agent_kind: kind }} />);
    expect(container.querySelector(`[data-agent="${kind ?? "shell"}"]`)).toHaveClass("size-6", "place-items-center");
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
