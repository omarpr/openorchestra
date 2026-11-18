import { ChevronLeft } from "lucide-react";
import { AgentIcon } from "@/components/AgentIcon";
import { StatusPill } from "@/components/StatusPill";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { agentName } from "@/lib/agents";
import type { Agent } from "@/lib/api";
import { navigate } from "@/lib/router";

/** Title row: back (phone), what and where the agent is, its status, and Fit to screen. */
export function AgentHeader({
  id,
  agent,
  isTablet,
  fit,
}: {
  id: string;
  agent?: Agent;
  isTablet: boolean;
  /** Shown in the terminal view only. */
  fit?: { on: boolean; set(on: boolean): void; disabled: boolean };
}) {
  return (
    <header className="flex h-14 shrink-0 items-center gap-1 border-b pr-2">
      {!isTablet && (
        <Button variant="ghost" size="icon" aria-label="Back to agents" onClick={() => navigate("#/")}>
          <ChevronLeft />
        </Button>
      )}
      {agent?.agent_kind && <AgentIcon kind={agent.agent_kind} className={isTablet ? "ml-4" : ""} />}
      <h2
        className={`flex min-w-0 flex-1 items-baseline font-semibold ${isTablet && !agent ? "pl-4" : "pl-1"}`}
        title={agent?.title ?? id}
      >
        {agent ? <Where agent={agent} /> : <span className="truncate">{id}</span>}
      </h2>
      {agent && <StatusPill status={agent.status} />}
      {fit && (
        <label
          htmlFor="fit-to-screen"
          className="flex min-h-11 min-w-11 cursor-pointer items-center justify-center gap-2 pl-2 text-sm"
        >
          <span className="hidden sm:inline">Fit to screen</span>
          <Switch
            id="fit-to-screen"
            aria-label="Fit to screen"
            checked={fit.on}
            onCheckedChange={fit.set}
            disabled={fit.disabled}
          />
        </label>
      )}
    </header>
  );
}

/** "project · branch · Claude Code", each part truncating on its own. */
function Where({ agent }: { agent: Agent }) {
  const sep = <span className="shrink-0 whitespace-pre text-muted-foreground"> · </span>;
  return (
    <>
      <span className="min-w-0 max-w-[35%] shrink-0 truncate whitespace-pre">{agent.project}</span>
      {sep}
      <span className="min-w-0 truncate">{agent.branch ?? agent.worktree}</span>
      {sep}
      <span className="min-w-0 max-w-[35%] shrink-0 truncate whitespace-pre">
        {agentName(agent.agent_kind) || agent.title}
      </span>
    </>
  );
}
