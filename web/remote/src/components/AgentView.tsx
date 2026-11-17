import { useRef, useState } from "react";
import { AgentComposer } from "@/components/agent/AgentComposer";
import { AgentHeader } from "@/components/agent/AgentHeader";
import { AgentToolbar } from "@/components/agent/AgentToolbar";
import { ConnectionState } from "@/components/agent/ConnectionState";
import { NotShared } from "@/components/agent/NotShared";
import { ConversationView } from "@/components/ConversationView";
import { StatusBar } from "@/components/StatusBar";
import { type TerminalHandle, TerminalView } from "@/components/TerminalView";
import { useClosures } from "@/hooks/useClosures";
import { useFocusWhenWaiting } from "@/hooks/useFocusWhenWaiting";
import { useIsTablet } from "@/hooks/useMedia";
import { useReadOnly } from "@/hooks/useReadOnly";
import { useTerminalPrefs } from "@/hooks/useTerminalPrefs";
import { useViewChoice, type View } from "@/hooks/useViewChoice";
import { agentName, isShell } from "@/lib/agents";
import type { Agent } from "@/lib/api";
import { keyBytes } from "@/lib/keys";

export function AgentView({
  id,
  agent,
  agentsLoaded = false,
  remoteUp = true,
}: {
  id: string;
  agent?: Agent;
  agentsLoaded?: boolean;
  /** The agent list's connection is live and remote control is on. */
  remoteUp?: boolean;
}) {
  const isTablet = useIsTablet();
  const term = useRef<TerminalHandle>(null);
  const replyInput = useRef<HTMLInputElement>(null);
  const [ctrl, setCtrl] = useState(false);
  const [initReadOnly, setInitReadOnly] = useState(false);
  const meReadOnly = useReadOnly();
  const prefs = useTerminalPrefs(id);
  const { view, pick, hasConv, notFound, loading } = useViewChoice(id, agent, agentsLoaded);
  const { closed, connected, setConnected, report, retry, attempt } = useClosures(view, remoteUp);

  const shell = agent ? isShell(agent) : false;
  const stopped = closed === "stopped" || agent?.status === "stopped";
  // Controls stay hidden until /api/me confirms the device may type.
  const readOnly = meReadOnly !== false || initReadOnly;
  const canType = !readOnly && !stopped && !closed && !loading && !notFound;
  useFocusWhenWaiting(agent?.status, view === "conversation" && canType, replyInput);

  const pickView = (v: View) => {
    pick(v);
    setConnected(true);
  };

  return (
    <div className="flex h-full min-h-0 flex-col" data-agent-id={id}>
      <AgentHeader
        id={id}
        agent={agent}
        isTablet={isTablet}
        fit={view === "terminal" ? { on: prefs.fit, set: prefs.setFit, disabled: readOnly } : undefined}
      />
      <AgentToolbar
        view={view}
        onPickView={pickView}
        canPick={hasConv}
        fontSize={prefs.fontSize}
        onStepFont={prefs.stepFont}
        watchOnly={meReadOnly !== null && readOnly}
      />

      <ConnectionState closed={closed} connected={connected} shell={shell} onRetry={retry} />

      <div className="min-h-0 flex-1">
        {notFound ? (
          <NotShared showBack={!isTablet} />
        ) : loading ? null : view === "conversation" ? (
          <ConversationView key={attempt} id={id} onClosed={report} onConnection={setConnected} />
        ) : (
          <TerminalView
            key={attempt}
            ref={term}
            id={id}
            fit={prefs.fit}
            fontSize={prefs.fontSize}
            ctrl={ctrl}
            onCtrlUsed={() => setCtrl(false)}
            onInit={setInitReadOnly}
            onClosed={report}
            onConnection={setConnected}
          />
        )}
      </div>

      {agent && view === "conversation" && !closed && <StatusBar status={agent.status} tool={agent.tool} />}

      {canType && (
        <AgentComposer
          id={id}
          name={shell ? null : agentName(agent?.agent_kind ?? null)}
          stopped={stopped}
          inputRef={replyInput}
          terminal={
            view === "terminal"
              ? {
                  hideKeys: prefs.hideKeys,
                  toggleKeys: prefs.toggleKeys,
                  ctrl,
                  toggleCtrl: () => setCtrl((c) => !c),
                  sendKey: (k) => term.current?.input(keyBytes(k)),
                }
              : undefined
          }
        />
      )}
    </div>
  );
}
