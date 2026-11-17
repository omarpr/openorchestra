import { ScrollArea } from "@/components/ui/scroll-area";
import type { Agent } from "@/lib/api";
import { groupAgents } from "@/lib/groups";
import { AgentCard } from "./AgentCard";
import { EmptyState } from "./EmptyState";

export function AgentList({ agents, selectedId }: { agents: Agent[]; selectedId?: string }) {
  if (agents.length === 0) {
    return (
      <EmptyState title="Nothing shared yet" compact>
        On your Mac, click Remote at the top of a tab to share it here.
      </EmptyState>
    );
  }
  // A lone agent needs no header; its card names the project and task.
  if (agents.length === 1) {
    const a = agents[0];
    return (
      <ScrollArea className="h-full">
        <ul className="flex flex-col gap-2 p-3">
          <li>
            <AgentCard agent={a} selected={a.id === selectedId} />
          </li>
        </ul>
      </ScrollArea>
    );
  }
  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-3 p-2">
        {groupAgents(agents).map((g) => {
          const label = [g.project, g.worktree].filter(Boolean).join(" · ") || "Other";
          return (
            <section key={g.key} aria-label={label} className="flex flex-col gap-1">
              <h2 className="flex items-baseline gap-1.5 px-1.5 pt-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                <span className="truncate">{label}</span>
                <span className="shrink-0 tabular-nums opacity-70">{g.agents.length}</span>
              </h2>
              <ul className="flex flex-col gap-1">
                {g.agents.map((a) => (
                  <li key={a.id}>
                    <AgentCard agent={a} selected={a.id === selectedId} showWhere={false} />
                  </li>
                ))}
              </ul>
            </section>
          );
        })}
      </div>
    </ScrollArea>
  );
}
