import { type ReactNode, useRef, useState, useSyncExternalStore } from "react";
import { AgentList } from "@/components/AgentList";
import { AgentView } from "@/components/AgentView";
import { ConnectionBanner } from "@/components/ConnectionBanner";
import { InstallFirst } from "@/components/InstallFirst";
import { TabletLayout } from "@/components/layout/TabletLayout";
import { TopBar } from "@/components/layout/TopBar";
import { NoSelection } from "@/components/NoSelection";
import { PairScreen } from "@/components/PairScreen";
import { Toaster } from "@/components/ui/sonner";
import { ViewportDebug } from "@/components/ViewportDebug";
import { useAgents } from "@/hooks/useAgents";
import { useLeaveUnshared } from "@/hooks/useLeaveUnshared";
import { useIsTablet } from "@/hooks/useMedia";
import { useVisualViewport } from "@/hooks/useVisualViewport";
import { useRoute } from "@/lib/router";
import { isIOS, isStandalone } from "@/lib/standalone";
import { getToken, subscribeToken } from "@/lib/token";

/** Paired: the agent list and the open agent, side by side on a tablet, one at a time on a phone. */
function Main() {
  const { agents, connected, disabled, untrusted, loaded, retry } = useAgents();
  const isTablet = useIsTablet();
  const route = useRoute();
  const shell = useRef<HTMLDivElement>(null);
  useVisualViewport(shell);
  const selectedId = route.name === "agent" ? route.id : undefined;
  const selectedAgent = agents.find((a) => a.id === selectedId);
  useLeaveUnshared(selectedId, selectedAgent, loaded);

  const detail = selectedId && (
    <AgentView
      key={selectedId}
      id={selectedId}
      agent={selectedAgent}
      agentsLoaded={loaded}
      remoteUp={connected && !disabled}
    />
  );

  let body: ReactNode;
  if (isTablet) {
    body = (
      <TabletLayout
        list={agents.length > 0 ? <AgentList agents={agents} selectedId={selectedId} /> : null}
        detail={detail || <NoSelection agents={agents} />}
      />
    );
  } else if (detail) {
    body = <div className="min-h-0 flex-1">{detail}</div>;
  } else {
    body = (
      <>
        <TopBar />
        <div className="min-h-0 flex-1">
          <AgentList agents={agents} />
        </div>
      </>
    );
  }

  return (
    <div
      ref={shell}
      className="oo-shell flex flex-col pt-[env(safe-area-inset-top)] pl-[env(safe-area-inset-left)] pr-[env(safe-area-inset-right)]"
    >
      <ConnectionBanner connected={connected} disabled={disabled} untrusted={untrusted} onRetry={retry} />
      {body}
      <ViewportDebug />
    </div>
  );
}

export default function App() {
  const token = useSyncExternalStore(subscribeToken, getToken);
  const [pairHere, setPairHere] = useState(false);
  const installFirst = !token && !pairHere && isIOS() && !isStandalone();
  return (
    <>
      {token ? <Main /> : installFirst ? <InstallFirst onPairHere={() => setPairHere(true)} /> : <PairScreen />}
      <Toaster position="top-center" />
    </>
  );
}
