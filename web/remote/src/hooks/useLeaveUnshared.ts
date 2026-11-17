import { useEffect, useRef } from "react";
import { toast } from "sonner";
import type { Agent } from "@/lib/api";
import { navigate } from "@/lib/router";

/**
 * The open tab was shared and is now gone from the list (unmarked on the Mac, or closed): say
 * so and go back to the list, rather than leaving a dead "not found" screen behind.
 */
export function useLeaveUnshared(selectedId: string | undefined, agent: Agent | undefined, loaded: boolean) {
  const last = useRef<Agent | undefined>(undefined);
  useEffect(() => {
    if (agent) {
      last.current = agent;
      return;
    }
    const was = last.current;
    if (!selectedId || !loaded || was?.id !== selectedId) return;
    last.current = undefined;
    toast(`“${was.title}” is no longer shared`, {
      description: "Remote control was turned off for it on your Mac, or the tab was closed.",
    });
    navigate("#/");
  }, [selectedId, agent, loaded]);
}
