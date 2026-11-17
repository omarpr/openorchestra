import { useEffect, useState } from "react";
import type { Agent } from "@/lib/api";
import { agentHref } from "@/lib/router";
import { timeSince } from "@/lib/time";
import { cn } from "@/lib/utils";
import { AgentIcon } from "./AgentIcon";
import { StatusPill } from "./StatusPill";

/** `showWhere` off when a group header above already names the project and task. */
export function AgentCard({
  agent,
  selected = false,
  showWhere = true,
}: {
  agent: Agent;
  selected?: boolean;
  showWhere?: boolean;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const since = timeSince(agent.status_since, now);
  const where = showWhere ? [agent.project, agent.worktree].filter(Boolean).join(" · ") : "";
  return (
    <a
      href={agentHref(agent.id)}
      aria-current={selected ? "page" : undefined}
      className={cn(
        "flex flex-col justify-center rounded-xl border bg-card text-card-foreground transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        // Inside a group the header names the place: a slimmer row.
        showWhere ? "min-h-16 gap-1 px-3 py-2 shadow-sm" : "min-h-12 gap-0.5 rounded-lg px-2.5 py-1.5",
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
      {(where || agent.tool || since) && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span className="truncate">{where}</span>
          <span className="ml-auto flex shrink-0 items-center gap-2">
            {agent.tool && <span className="max-w-28 truncate font-mono">{agent.tool}</span>}
            {since && <span>{since}</span>}
          </span>
        </div>
      )}
    </a>
  );
}
