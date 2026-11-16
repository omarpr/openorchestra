import type { Agent } from "@/lib/api";
import { ScrollArea } from "@/components/ui/scroll-area";
import { AgentCard } from "./AgentCard";

export function AgentList({ agents, selectedId }: { agents: Agent[]; selectedId?: string }) {
  if (agents.length === 0) {
    return (
      <div className="p-6 text-center text-sm text-muted-foreground">
        No agents are shared yet. Mark a tab as remote on your Mac (Settings → Remote).
      </div>
    );
  }
  return (
    <ScrollArea className="h-full">
      <ul className="flex flex-col gap-2 p-3">
        {agents.map((a) => (
          <li key={a.id}>
            <AgentCard agent={a} selected={a.id === selectedId} />
          </li>
        ))}
      </ul>
    </ScrollArea>
  );
}
