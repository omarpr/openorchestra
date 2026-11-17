import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Agent } from "@/lib/api";
import { groupAgents } from "@/lib/groups";
import { AgentList } from "./AgentList";

const agent = (id: string, project: string, worktree: string, title = id): Agent => ({
  id,
  title,
  agent_kind: "claude-code",
  project,
  worktree,
  branch: null,
  has_conversation: false,
  status: "running",
  status_since: null,
  tool: null,
});

describe("groupAgents", () => {
  it("groups by project and task, in first-seen order", () => {
    const groups = groupAgents([
      agent("a", "web", "login"),
      agent("b", "api", "main"),
      agent("c", "web", "login"),
      agent("d", "web", "main"),
    ]);
    expect(groups.map((g) => [g.project, g.worktree, g.agents.map((a) => a.id)])).toEqual([
      ["web", "login", ["a", "c"]],
      ["api", "main", ["b"]],
      ["web", "main", ["d"]],
    ]);
  });
});

describe("AgentList", () => {
  it("puts tabs of the same project and task under one header", () => {
    render(
      <AgentList
        agents={[
          agent("a", "web", "login", "Claude"),
          agent("b", "web", "login", "Shell"),
          agent("c", "api", "main", "Codex"),
        ]}
      />,
    );
    const web = screen.getByRole("region", { name: "web · login" });
    expect(within(web).getByText("Claude")).toBeInTheDocument();
    expect(within(web).getByText("Shell")).toBeInTheDocument();
    expect(within(web).getByText("2")).toBeInTheDocument();
    // The header names the place; cards don't repeat it.
    expect(within(web).queryByText("web · login", { selector: "a *" })).not.toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "api · main" })).getByText("Codex")).toBeInTheDocument();
  });

  it("shows a lone agent without a header", () => {
    render(<AgentList agents={[agent("a", "web", "login", "Claude")]} />);
    expect(screen.queryByRole("region")).not.toBeInTheDocument();
    expect(screen.getByText("web · login")).toBeInTheDocument();
  });
});
