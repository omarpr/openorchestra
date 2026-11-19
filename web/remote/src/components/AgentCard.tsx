import { useEffect, useState } from "react";
import { tabTitle } from "@/lib/agents";
import type { Agent } from "@/lib/api";
import { agentHref } from "@/lib/router";
import { timeSince } from "@/lib/time";
import { cn } from "@/lib/utils";
import { AgentIcon } from "./AgentIcon";
import { StatusPill } from "./StatusPill";

/** Grouped cards use the group heading for their project and task. */
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
  const since = agent.status === "running" || agent.status === "waiting" ? timeSince(agent.status_since, now) : "";
  const title = tabTitle(agent);
  const branch = agent.branch !== agent.worktree && agent.branch !== title ? agent.branch : null;
  const where = (showWhere ? [agent.project, agent.worktree, branch] : [branch]).filter(Boolean).join(" · ");
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
        <span className="flex min-w-0 items-center gap-2">
          <AgentIcon kind={agent.agent_kind} />
          <span className="truncate font-medium">{title}</span>
        </span>
        <span className="flex shrink-0 items-center gap-1.5 whitespace-nowrap">
          <StatusPill status={agent.status} />
          {since && (
            <time
              dateTime={agent.status_since ?? undefined}
              title="Time in this state"
              className="text-xs text-muted-foreground tabular-nums"
            >
              {since}
            </time>
          )}
        </span>
      </div>
      {(where || agent.tool) && (
        <div className="flex items-center gap-2 pl-8 text-xs text-muted-foreground">
          {where && <span className="truncate">{where}</span>}
          {agent.tool && <span className="ml-auto max-w-28 shrink-0 truncate font-mono">{agent.tool}</span>}
        </div>
      )}
    </a>
  );
}
