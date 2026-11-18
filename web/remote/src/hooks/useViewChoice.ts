import { useCallback, useState } from "react";
import type { Agent } from "@/lib/api";
import { readPref, writePref } from "@/lib/storage";

export type View = "conversation" | "terminal";

/**
 * Remember explicit view choices. A pending transcript uses Terminal temporarily.
 */
export function useViewChoice(id: string, agent: Agent | undefined, agentsLoaded: boolean) {
  const hasConv = agent?.has_conversation === true;
  const canPick = hasConv || agent?.agent_kind === "claude-code" || agent?.agent_kind === "codex";
  const [saved, setSaved] = useState<View | null>(() => {
    const v = readPref(`oo.view.${id}`);
    return v === "conversation" || v === "terminal" ? v : null;
  });
  const pick = useCallback(
    (v: View) => {
      setSaved(v);
      writePref(`oo.view.${id}`, v);
    },
    [id],
  );

  const notFound = !agent && agentsLoaded;
  return {
    view: (canPick && saved ? saved : hasConv ? "conversation" : "terminal") as View,
    canPick,
    pick,
    hasConv,
    notFound,
    // A terminal can open before the list arrives; a conversation needs the agent first.
    loading: !agent && !notFound && saved !== "terminal",
  };
}
