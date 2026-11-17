import { useCallback, useEffect, useState } from "react";
import type { Agent } from "@/lib/api";
import { readPref, writePref } from "@/lib/storage";

export type View = "conversation" | "terminal";

/**
 * Which view an agent shows, remembered per agent. Without a conversation it is always the
 * terminal; the first time that happens it is remembered, so a conversation appearing later
 * doesn't pull the user off the terminal they are using.
 */
export function useViewChoice(id: string, agent: Agent | undefined, agentsLoaded: boolean) {
  const hasConv = agent?.has_conversation === true;
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

  const noConvYet = !!agent && !hasConv && saved === null;
  useEffect(() => {
    if (noConvYet) pick("terminal");
  }, [noConvYet, pick]);

  const notFound = !agent && agentsLoaded;
  return {
    view: (hasConv ? (saved ?? "conversation") : "terminal") as View,
    pick,
    hasConv,
    notFound,
    // A terminal can open before the list arrives; a conversation needs the agent first.
    loading: !agent && !notFound && saved !== "terminal",
  };
}
