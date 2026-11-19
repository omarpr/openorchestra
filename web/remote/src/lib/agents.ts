import type { Agent } from "./api";

const AGENTS: Record<string, { name: string; conversationWhilePending?: boolean }> = {
  "claude-code": { name: "Claude Code", conversationWhilePending: true },
  pi: { name: "pi" },
  codex: { name: "Codex", conversationWhilePending: true },
  kimi: { name: "Kimi" },
  grok: { name: "Grok" },
  opencode: { name: "OpenCode", conversationWhilePending: true },
};

/** Display name for an agent kind; "" for a shell. */
export const agentName = (kind: string | null) => (kind ? (AGENTS[kind]?.name ?? kind) : "");

/** Whether the switch is available before an agent publishes its first transcript. */
export const canOpenConversation = (agent: Agent | undefined) =>
  agent?.has_conversation === true || AGENTS[agent?.agent_kind ?? ""]?.conversationWhilePending === true;

/** A shell tab: no agent runs there. */
export const isShell = (agent: Pick<Agent, "agent_kind">) => agent.agent_kind === null;

/** The desktop shell title includes a prompt glyph; the web app has its own icon. */
export function tabTitle(agent: Pick<Agent, "agent_kind" | "title">): string {
  return isShell(agent) ? agent.title.replace(/^❯_\s*/, "") || "Shell" : agent.title;
}

/** Why a conversation/terminal stream ended, as the server names it. */
export type Closure = "stopped" | "no_conversation" | "unmarked" | "disabled" | "revoked" | (string & {});

const CLOSED_COPY: Record<string, string> = {
  stopped: "Agent stopped",
  no_conversation: "The conversation isn't available. Use the terminal.",
  unmarked: "Remote control was turned off for this agent",
  disabled: "Remote control is off on the Mac",
  revoked: "This device was removed. Pair again.",
};

export function closureText(reason: Closure, shell: boolean): string {
  if (reason === "stopped" && shell) return "Shell closed";
  return CLOSED_COPY[reason] ?? "Disconnected";
}

/** Closures that end both views at once (the rest belong to the view that reported them). */
export const isRemoteWide = (r: Closure) => r === "unmarked" || r === "revoked" || r === "disabled";

/** Closures a new connection can clear; the rest need the user or the Mac to act. */
export const isRetryable = (r: Closure) => r !== "revoked" && r !== "stopped" && r !== "no_conversation";
