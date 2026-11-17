import type { Agent } from "./api";

const AGENT_NAMES: Record<string, string> = {
  "claude-code": "Claude Code",
  pi: "pi",
  codex: "Codex",
  kimi: "Kimi",
  opencode: "OpenCode",
};

/** Display name for an agent kind; "" for a shell. */
export const agentName = (kind: string | null) => (kind ? (AGENT_NAMES[kind] ?? kind) : "");

/** A shell tab: no agent runs there. */
export const isShell = (agent: Pick<Agent, "agent_kind">) => agent.agent_kind === null;

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
