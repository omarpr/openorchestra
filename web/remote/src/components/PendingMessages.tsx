import { Check, Clock, Loader2, RotateCcw, TriangleAlert, X } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { usePending } from "@/hooks/usePending";
import type { ConvEntry } from "@/lib/conversation";
import { arrived, type PendingMessage, removePending } from "@/lib/pending";
import { deliver } from "@/lib/send";
import { cn } from "@/lib/utils";

/** A message still unread this long gets a hint to look at the terminal. */
export const STUCK_MS = 3 * 60 * 1000;

/**
 * Messages sent from here that the transcript doesn't show yet, under the conversation. Each
 * disappears when the agent's own copy of it arrives (the real bubble takes its place).
 */
export function PendingMessages({
  agentId,
  entries,
  shell,
  onOpenTerminal,
}: {
  agentId: string;
  entries: ConvEntry[];
  shell: boolean;
  onOpenTerminal?: () => void;
}) {
  const pending = usePending(agentId);
  const now = useNow(pending.length > 0);

  useEffect(() => {
    removePending(arrived(pending, entries));
  }, [pending, entries]);

  if (pending.length === 0) return null;
  return (
    <ul className="flex flex-col gap-3" aria-label="Messages not read yet">
      {pending.map((m) => (
        <li key={m.id}>
          <PendingBubble m={m} stuck={now - m.since >= STUCK_MS} shell={shell} onOpenTerminal={onOpenTerminal} />
        </li>
      ))}
    </ul>
  );
}

function PendingBubble({
  m,
  stuck,
  shell,
  onOpenTerminal,
}: {
  m: PendingMessage;
  stuck: boolean;
  shell: boolean;
  onOpenTerminal?: () => void;
}) {
  const failed = m.state === "failed";
  return (
    <div className="flex flex-col items-end gap-1">
      <div
        className={cn(
          "max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-sm px-3 py-2 text-sm",
          failed
            ? "border border-destructive/60 bg-destructive/10 text-foreground"
            : "bg-primary/60 text-primary-foreground",
        )}
      >
        {m.text}
      </div>
      <Status m={m} stuck={stuck} shell={shell} onOpenTerminal={onOpenTerminal} />
    </div>
  );
}

function Status({
  m,
  stuck,
  shell,
  onOpenTerminal,
}: {
  m: PendingMessage;
  stuck: boolean;
  shell: boolean;
  onOpenTerminal?: () => void;
}) {
  const line = "flex items-center gap-1 text-xs text-muted-foreground";
  const who = shell ? "the shell" : "the agent";
  if (m.state === "sending") {
    return (
      <p role="status" className={line}>
        <Loader2 aria-hidden className="size-3 animate-spin" /> Sending…
      </p>
    );
  }
  if (m.state === "failed") {
    return (
      <div role="alert" className={cn(line, "text-destructive")}>
        <TriangleAlert aria-hidden className="size-3" />
        <span>Not sent: {m.error}</span>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-2"
          onClick={() => void deliver(m.id, m.agentId, m.text, shell)}
        >
          <RotateCcw aria-hidden className="size-3" /> Retry
        </Button>
        <Button variant="ghost" size="sm" className="h-7 px-2" onClick={() => removePending([m.id])}>
          <X aria-hidden className="size-3" /> Discard
        </Button>
      </div>
    );
  }
  if (stuck) {
    return (
      <div role="status" className={line}>
        <Clock aria-hidden className="size-3" />
        <span>Not picked up yet. Check the terminal.</span>
        {onOpenTerminal && (
          <Button variant="ghost" size="sm" className="h-7 px-2" onClick={onOpenTerminal}>
            Open terminal
          </Button>
        )}
      </div>
    );
  }
  return (
    <p role="status" className={line}>
      <Check aria-hidden className="size-3" />
      {m.queued ? `Sent · queued, ${who} reads it after the current step` : `Sent · waiting for ${who}`}
    </p>
  );
}

/** The current time, ticking every 15 s while `active` (for the "stuck" hint). */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(t);
  }, [active]);
  return now;
}
