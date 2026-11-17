import { useCallback, useSyncExternalStore } from "react";
import { type PendingMessage, pendingFor, subscribePending } from "@/lib/pending";

/** This agent's messages that its transcript doesn't show yet. */
export function usePending(agentId: string): PendingMessage[] {
  const snapshot = useCallback(() => pendingFor(agentId), [agentId]);
  return useSyncExternalStore(subscribePending, snapshot);
}
