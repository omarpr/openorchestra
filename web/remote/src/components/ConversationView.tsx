import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { EntryRow } from "@/components/EntryRow";
import { PendingMessages } from "@/components/PendingMessages";
import { type ToolRef, ToolSheet } from "@/components/ToolSheet";
import { usePending } from "@/hooks/usePending";
import { useConversation } from "@/lib/conversation";

const STICK_PX = 80;

interface Props {
  id: string;
  onClosed?(reason: string): void;
  onConnection?(connected: boolean): void;
  /** A shell's conversation (wording of pending messages). */
  shell?: boolean;
  /** Switch to the terminal (offered when a message isn't picked up). */
  onOpenTerminal?(): void;
}

export function ConversationView({ id, onClosed, onConnection, shell = false, onOpenTerminal }: Props) {
  const { entries, connected, closed } = useConversation(id);
  const scroller = useRef<HTMLDivElement>(null);
  const stuck = useRef(true);
  const [unseen, setUnseen] = useState(false);
  const [tool, setTool] = useState<ToolRef | null>(null);
  const everConnected = useRef(false);
  const cb = useRef({ onClosed, onConnection });
  cb.current = { onClosed, onConnection };

  useEffect(() => {
    if (connected) everConnected.current = true;
    if (connected || everConnected.current) cb.current.onConnection?.(connected);
  }, [connected]);
  useEffect(() => {
    if (closed) cb.current.onClosed?.(closed);
  }, [closed]);

  // Sending a message shows it at the bottom: go there.
  const pendingCount = usePending(id).length;
  const lastPending = useRef(pendingCount);
  useLayoutEffect(() => {
    const el = scroller.current;
    const sent = pendingCount > lastPending.current;
    lastPending.current = pendingCount;
    if (!el || !(sent || stuck.current)) return;
    el.scrollTop = el.scrollHeight;
    stuck.current = true;
  }, [pendingCount]);

  const seen = useRef<Set<string>>(new Set());
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const fresh = entries.some((e) => !seen.current.has(e.id));
    seen.current = new Set(entries.map((e) => e.id));
    if (stuck.current) {
      el.scrollTop = el.scrollHeight;
    } else if (fresh) {
      setUnseen(true);
    }
  }, [entries]);

  // The scroller itself resizes (keyboard opens, status bar appears): keep the bottom in view while stuck.
  useEffect(() => {
    const el = scroller.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stuck.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_PX;
    stuck.current = atBottom;
    if (atBottom) setUnseen(false);
  };
  const jump = () => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
    stuck.current = true;
    setUnseen(false);
  };

  // Live status of the open tool: a sheet opened while it ran fetches once it finishes.
  const toolPending = !!tool && entries.find((e) => e.tool?.id === tool.id)?.tool?.status === "pending";

  return (
    <div className="relative flex h-full min-h-0 flex-col">
      <div
        ref={scroller}
        data-testid="conversation-scroll"
        onScroll={onScroll}
        className="min-h-0 flex-1 space-y-3 overflow-y-auto overscroll-contain px-3 py-3"
      >
        {entries.map((e) => (
          <EntryRow key={e.id} e={e} onTool={setTool} />
        ))}
        <PendingMessages agentId={id} entries={entries} shell={shell} onOpenTerminal={onOpenTerminal} />
      </div>
      {unseen && (
        <button
          type="button"
          onClick={jump}
          className="absolute bottom-3 left-1/2 min-h-11 -translate-x-1/2 rounded-full bg-primary px-4 text-sm font-medium text-primary-foreground shadow-lg"
        >
          New messages ↓
        </button>
      )}
      <ToolSheet agentId={id} tool={tool} pending={toolPending} onClose={() => setTool(null)} />
    </div>
  );
}
