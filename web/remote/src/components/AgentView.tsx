import { Suspense, useRef, useState } from "react";
import { AgentComposer } from "@/components/agent/AgentComposer";
import { AgentHeader } from "@/components/agent/AgentHeader";
import { AgentToolbar } from "@/components/agent/AgentToolbar";
import { ConnectionState } from "@/components/agent/ConnectionState";
import { NotShared } from "@/components/agent/NotShared";
import { ViewErrorBoundary } from "@/components/agent/ViewErrorBoundary";
import { ConversationView, TerminalView } from "@/components/lazyViews";
import { StatusBar } from "@/components/StatusBar";
import type { TerminalHandle } from "@/components/TerminalView";
import { useClosures } from "@/hooks/useClosures";
import { useDevice } from "@/hooks/useDevice";
import { useFocusWhenWaiting } from "@/hooks/useFocusWhenWaiting";
import { useIsTablet } from "@/hooks/useMedia";
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
  const replyInput = useRef<HTMLTextAreaElement>(null);
  const [ctrl, setCtrl] = useState(false);
  const [initReadOnly, setInitReadOnly] = useState(false);
  const device = useDevice();
  const prefs = useTerminalPrefs(id);
  const { view, pick, hasConv, canPick, notFound, loading } = useViewChoice(id, agent, agentsLoaded);
  const { closed, connected, setConnected, report, retry, attempt } = useClosures(view, remoteUp);
  const activity = closed ? undefined : agent;

  const shell = agent ? isShell(agent) : false;
  const stopped = closed === "stopped" || agent?.status === "stopped";
  // Controls stay hidden until /api/me confirms the device may type.
  const readOnly = device?.read_only !== false || initReadOnly;
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
        canPick={canPick}
        fontSize={prefs.fontSize}
        onStepFont={prefs.stepFont}
        watchOnly={device !== null && readOnly}
      />

      <ConnectionState closed={closed} connected={connected} shell={shell} onRetry={retry} />

      <div className="min-h-0 flex-1">
        <ViewErrorBoundary key={`${view}:${attempt}`}>
          <Suspense fallback={<ViewLoading />}>
            {notFound ? (
              <NotShared showBack={!isTablet} />
            ) : loading ? null : view === "conversation" ? (
              <AgentConversation
                ready={hasConv}
                id={id}
                onClosed={report}
                onConnection={setConnected}
                shell={shell}
                activity={activity}
                onOpenTerminal={() => pickView("terminal")}
              />
            ) : (
              <TerminalView
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
          </Suspense>
        </ViewErrorBoundary>
      </div>

      {canType && (
        <AgentComposer
          id={id}
          name={shell ? null : agentName(agent?.agent_kind ?? null)}
          stopped={stopped}
          voiceInput={device?.voice_input}
          inputRef={replyInput}
          track={hasConv ? { queued: agent?.status === "running" } : undefined}
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

function AgentConversation({
  ready,
  activity,
  ...props
}: React.ComponentProps<typeof ConversationView> & { ready: boolean }) {
  if (ready) return <ConversationView {...props} activity={activity} />;
  return (
    <div className="flex h-full flex-col">
      <div
        role="status"
        className="flex flex-1 items-center justify-center px-4 text-center text-sm text-muted-foreground"
      >
        Waiting for a transcript for this tab. You can use Terminal in the meantime.
      </div>
      {activity && <StatusBar status={activity.status} tool={activity.tool} />}
    </div>
  );
}

/** While a view's code downloads (first open only). */
function ViewLoading() {
  return (
    <div role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">
      Loading…
    </div>
  );
}
