import { useEffect, useRef, useState } from "react";
import { ChevronLeft, Keyboard } from "lucide-react";
import { me, type Agent } from "@/lib/api";
import { keyBytes, type Key } from "@/lib/keys";
import { navigate } from "@/lib/router";
import { useIsTablet } from "@/hooks/useMedia";
import { useVisualViewport } from "@/hooks/useVisualViewport";
import { KeyBar } from "@/components/KeyBar";
import { QuickReply } from "@/components/QuickReply";
import { StatusPill } from "@/components/StatusPill";
import { TerminalView, type TerminalHandle } from "@/components/TerminalView";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { AgentIcon } from "@/components/AgentIcon";
import { ConversationView } from "@/components/ConversationView";

const MIN_FONT = 9;
const MAX_FONT = 18;

const CLOSED_COPY: Record<string, string> = {
  stopped: "Agent stopped",
  unmarked: "Remote control was turned off for this agent",
  disabled: "Remote control is off on the Mac",
  revoked: "This device was removed. Pair again.",
};

const AGENT_NAMES: Record<string, string> = {
  "claude-code": "Claude Code",
  pi: "pi",
  codex: "Codex",
  kimi: "Kimi",
  opencode: "OpenCode",
};
export const agentName = (kind: string | null) => (kind ? (AGENT_NAMES[kind] ?? kind) : "");

type View = "conversation" | "terminal";

function load(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function save(key: string, v: string) {
  try {
    localStorage.setItem(key, v);
  } catch {
    /* per-device convenience only */
  }
}

export function AgentView({ id, agent }: { id: string; agent?: Agent }) {
  const isTablet = useIsTablet();
  const { bottom } = useVisualViewport();
  const term = useRef<TerminalHandle>(null);
  const [meReadOnly, setMeReadOnly] = useState<boolean | null>(null);
  const [initReadOnly, setInitReadOnly] = useState(false);
  const [fit, setFit] = useState(() => load(`oo.fit.${id}`) === "1");
  const [fontSize, setFontSize] = useState(() => {
    const n = Number(load("oo.fontSize"));
    return n >= MIN_FONT && n <= MAX_FONT ? n : 12;
  });
  const [hideKeys, setHideKeys] = useState(() => load("oo.hideKeys") === "1");
  const [ctrl, setCtrl] = useState(false);
  const [closed, setClosed] = useState<string | null>(null);
  const [connected, setConnected] = useState(true);
  const hasConv = agent?.has_conversation === true;
  const [savedView, setSavedView] = useState<View | null>(() => {
    const v = load(`oo.view.${id}`);
    return v === "conversation" || v === "terminal" ? v : null;
  });
  const view: View = hasConv ? (savedView ?? "conversation") : "terminal";
  const pickView = (v: View) => {
    setSavedView(v);
    save(`oo.view.${id}`, v);
    setConnected(true);
  };

  useEffect(() => {
    let live = true;
    me()
      .then((m) => live && setMeReadOnly(m.read_only))
      .catch(() => {
        /* 401 already cleared the token; other errors keep controls hidden */
      });
    return () => {
      live = false;
    };
  }, []);

  const stopped = closed === "stopped" || agent?.status === "stopped";
  // Controls stay hidden until /api/me confirms the device may type.
  const readOnly = meReadOnly !== false || initReadOnly;
  const canType = !readOnly && !stopped && !closed;
  const where = agent ? (agent.branch ?? agent.worktree) : "";

  const setFitPersist = (on: boolean) => {
    setFit(on);
    save(`oo.fit.${id}`, on ? "1" : "0");
  };
  const step = (d: number) =>
    setFontSize((f) => {
      const n = Math.min(MAX_FONT, Math.max(MIN_FONT, f + d));
      save("oo.fontSize", String(n));
      return n;
    });
  const toggleKeys = () =>
    setHideKeys((h) => {
      save("oo.hideKeys", h ? "0" : "1");
      return !h;
    });
  const sendKey = (k: Key) => term.current?.input(keyBytes(k));

  return (
    <div className="flex h-full min-h-0 flex-col" data-agent-id={id}>
      <header className="flex h-14 shrink-0 items-center gap-1 border-b pr-2">
        {!isTablet && (
          <Button variant="ghost" size="icon" aria-label="Back to agents" onClick={() => navigate("#/")}>
            <ChevronLeft />
          </Button>
        )}
        {agent && <AgentIcon kind={agent.agent_kind} className={isTablet ? "ml-4" : ""} />}
        <h2
          className={`flex min-w-0 flex-1 items-baseline font-semibold ${isTablet && !agent ? "pl-4" : "pl-1"}`}
          title={agent?.title ?? id}
        >
          {agent ? (
            <>
              <span className="shrink-0 whitespace-pre">{agent.project}</span>
              <span className="shrink-0 whitespace-pre text-muted-foreground"> · </span>
              <span className="min-w-0 truncate">{where}</span>
              <span className="shrink-0 whitespace-pre text-muted-foreground"> · </span>
              <span className="shrink-0 whitespace-pre">{agentName(agent.agent_kind)}</span>
            </>
          ) : (
            <span className="truncate">{id}</span>
          )}
        </h2>
        {agent && <StatusPill status={agent.status} />}
        {view === "terminal" && (
          <label className="flex min-h-11 min-w-11 cursor-pointer items-center justify-center gap-2 pl-2 text-sm">
            <span className="hidden sm:inline">Fit to phone</span>
            <Switch aria-label="Fit to phone" checked={fit} onCheckedChange={setFitPersist} disabled={readOnly} />
          </label>
        )}
      </header>

      <div className="flex min-h-11 shrink-0 items-center gap-1 border-b px-2 text-sm">
        {hasConv && (
          <Tabs value={view} onValueChange={(v) => pickView(v as View)}>
            <TabsList>
              <TabsTrigger value="conversation">Conversation</TabsTrigger>
              <TabsTrigger value="terminal">Terminal</TabsTrigger>
            </TabsList>
          </Tabs>
        )}
        {view === "terminal" && (<>
        <Button variant="ghost" size="icon" aria-label="Smaller text" disabled={fontSize <= MIN_FONT} onClick={() => step(-1)}>
          A−
        </Button>
        <span className="w-8 text-center tabular-nums text-muted-foreground" aria-live="polite">
          {fontSize}
        </span>
        <Button variant="ghost" size="icon" aria-label="Larger text" disabled={fontSize >= MAX_FONT} onClick={() => step(1)}>
          A+
        </Button>
        </>)}
        <div className="flex-1" />
        {meReadOnly !== null && readOnly && <Badge variant="secondary">Watch only</Badge>}
      </div>

      {closed ? (
        <div role="status" className="shrink-0 border-b bg-amber-100 px-4 py-2 text-sm text-amber-950 dark:bg-amber-950 dark:text-amber-100">
          {CLOSED_COPY[closed] ?? "Disconnected"}
        </div>
      ) : (
        !connected && (
          <div role="status" className="shrink-0 border-b bg-muted px-4 py-1.5 text-sm text-muted-foreground">
            Reconnecting…
          </div>
        )
      )}

      <div className="min-h-0 flex-1" style={{ paddingBottom: canType ? bottom : 0 }}>
        {view === "conversation" ? (
          <ConversationView id={id} onClosed={setClosed} onConnection={setConnected} />
        ) : (
          <TerminalView
            ref={term}
            id={id}
            fit={fit}
            fontSize={fontSize}
            ctrl={ctrl}
            onCtrlUsed={() => setCtrl(false)}
            onInit={setInitReadOnly}
            onClosed={setClosed}
            onConnection={setConnected}
          />
        )}
      </div>

      {/* Task 6: the conversation status bar plugs in here, between the list and the reply box. */}

      {canType && (
        <div className="shrink-0 border-t bg-background" style={{ transform: `translateY(${-bottom}px)` }}>
          {view === "terminal" && !hideKeys && <KeyBar ctrl={ctrl} onToggleCtrl={() => setCtrl((c) => !c)} onKey={sendKey} />}
          <div className="flex items-center gap-2 px-2 pb-2">
            <QuickReply id={id} disabled={stopped} />
            {view === "terminal" && (
            <Button
              variant="outline"
              size="icon"
              aria-label={hideKeys ? "Show keys" : "Hide keys"}
              aria-pressed={!hideKeys}
              onClick={toggleKeys}
            >
              <Keyboard />
            </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
