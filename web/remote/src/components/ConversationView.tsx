import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useConversation } from "@/lib/conversation";
import { EntryRow } from "@/components/EntryRow";
import { ToolSheet, type ToolRef } from "@/components/ToolSheet";

const STICK_PX = 80;

interface Props {
  id: string;
  onClosed?(reason: string): void;
  onConnection?(connected: boolean): void;
}

export function ConversationView({ id, onClosed, onConnection }: Props) {
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

  const first = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (stuck.current) {
      el.scrollTop = el.scrollHeight;
    } else if (!first.current) {
      setUnseen(true);
    }
    first.current = false;
  }, [entries]);

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
      <ToolSheet agentId={id} tool={tool} onClose={() => setTool(null)} />
    </div>
  );
}
