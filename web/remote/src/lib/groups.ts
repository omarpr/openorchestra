import type { Agent } from "./api";

export interface AgentGroup {
  key: string;
  project: string;
  worktree: string;
  agents: Agent[];
}

/**
 * Agents grouped by project + worktree (task), keeping `list`'s order: each
 * group sits where its first agent was, so a group holding an agent that
 * needs input stays on top of a status-sorted list.
 */
export function groupAgents(list: Agent[]): AgentGroup[] {
  const groups = new Map<string, AgentGroup>();
  for (const a of list) {
    const key = JSON.stringify([a.project, a.worktree]);
    let g = groups.get(key);
    if (!g) {
      g = { key, project: a.project, worktree: a.worktree, agents: [] };
      groups.set(key, g);
    }
    g.agents.push(a);
  }
  return [...groups.values()];
}
