import { useEffect, useState } from "react";
import type { Agent } from "@/lib/api";
import { agentHref } from "@/lib/router";
import { timeSince } from "@/lib/time";
import { cn } from "@/lib/utils";
import { AgentIcon } from "./AgentIcon";
import { StatusPill } from "./StatusPill";

export function AgentCard({ agent, selected = false }: { agent: Agent; selected?: boolean }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const since = timeSince(agent.status_since, now);
  const where = [agent.project, agent.worktree].filter(Boolean).join(" · ");
  return (
    <a
      href={agentHref(agent.id)}
      aria-current={selected ? "page" : undefined}
      className={cn(
        "flex min-h-16 flex-col justify-center gap-1 rounded-xl border bg-card px-3 py-2 text-card-foreground shadow-sm transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        selected && "bg-accent",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex min-w-0 items-center gap-1.5">
          <AgentIcon kind={agent.agent_kind} />
          <span className="truncate font-medium">{agent.title}</span>
        </span>
        <StatusPill status={agent.status} />
      </div>
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="truncate">{where}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          {agent.tool && <span className="max-w-28 truncate font-mono">{agent.tool}</span>}
          {since && <span>{since}</span>}
        </span>
      </div>
    </a>
  );
}
