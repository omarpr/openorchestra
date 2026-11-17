import { EmptyState } from "@/components/EmptyState";
import { Button } from "@/components/ui/button";
import type { Agent } from "@/lib/api";
import { agentHref } from "@/lib/router";

/** The tablet's right side before an agent is picked: what's shared, and a shortcut to what needs you. */
export function NoSelection({ agents }: { agents: Agent[] }) {
  const waiting = agents.find((a) => a.status === "waiting");
  const shared = agents.length;
  const summary =
    shared === 0
      ? "Tabs you mark for Remote on your Mac show up on the left."
      : `${shared} ${shared === 1 ? "tab is" : "tabs are"} shared from your Mac. Pick one to watch it or reply.`;
  return (
    <EmptyState
      title={shared === 0 ? "Nothing shared yet" : "Pick a tab"}
      action={
        waiting && (
          <Button asChild>
            <a href={agentHref(waiting.id)}>Open “{waiting.title}”, it needs you</a>
          </Button>
        )
      }
    >
      {summary}
    </EmptyState>
  );
}
