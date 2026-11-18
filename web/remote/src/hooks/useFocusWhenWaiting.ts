import { type RefObject, useEffect, useRef } from "react";
import type { AgentStatus } from "@/lib/api";

/** Focus `input` once each time the agent starts waiting for the user (while `active`). */
export function useFocusWhenWaiting(
  status: AgentStatus | undefined,
  active: boolean,
  input: RefObject<HTMLTextAreaElement>,
) {
  const focused = useRef(false);
  useEffect(() => {
    if (status !== "waiting") {
      focused.current = false;
      return;
    }
    if (!focused.current && active && input.current) {
      input.current.focus();
      focused.current = true;
    }
  }, [status, active, input]);
}
