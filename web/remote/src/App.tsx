import { useState, useSyncExternalStore } from "react";
import { PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { useAgents } from "@/hooks/useAgents";
import { useIsTablet } from "@/hooks/useMedia";
import { getToken, subscribeToken } from "@/lib/token";
import { useRoute } from "@/lib/router";
import { AgentList } from "@/components/AgentList";
import { AgentView } from "@/components/AgentView";
import { ConnectionBanner } from "@/components/ConnectionBanner";
import { PairScreen } from "@/components/PairScreen";
import { InstallFirst } from "@/components/InstallFirst";
import { isIOS, isStandalone } from "@/lib/standalone";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";

function Brand() {
  return (
    <>
      <img src="/logo-64.png" alt="" width={24} height={24} className="size-6 shrink-0 rounded-md" />
      <h1 className="font-semibold">openOrchestra Agents</h1>
    </>
  );
}

function Main() {
  const { agents, connected, disabled } = useAgents();
  const isTablet = useIsTablet();
  const route = useRoute();
  const [collapsed, setCollapsed] = useState(false);
  const selectedId = route.name === "agent" ? route.id : undefined;
  const selectedAgent = agents.find((a) => a.id === selectedId);

  const header = (
    <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4">
      <Brand />
    </header>
  );

  let body;
  if (isTablet) {
    body = (
      <div className="flex min-h-0 flex-1">
        {!collapsed && (
          <aside className="flex w-80 shrink-0 flex-col border-r" aria-label="Agents">
            <div className="flex h-14 shrink-0 items-center justify-between border-b pl-4 pr-1">
              <div className="flex min-w-0 items-center gap-2">
                <Brand />
              </div>
              <Button variant="ghost" size="icon" aria-label="Hide agent list" onClick={() => setCollapsed(true)}>
                <PanelLeftClose />
              </Button>
            </div>
            <div className="min-h-0 flex-1">
              <AgentList agents={agents} selectedId={selectedId} />
            </div>
          </aside>
        )}
        <section className="flex min-w-0 flex-1 flex-col" aria-label="Agent">
          {collapsed && (
            <div className="flex h-14 shrink-0 items-center border-b px-1">
              <Button variant="ghost" size="icon" aria-label="Show agent list" onClick={() => setCollapsed(false)}>
                <PanelLeftOpen />
              </Button>
            </div>
          )}
          <div className="min-h-0 flex-1">
            {selectedId ? (
              <AgentView key={selectedId} id={selectedId} agent={selectedAgent} />
            ) : (
              <div className="flex h-full items-center justify-center text-muted-foreground">Select an agent</div>
            )}
          </div>
        </section>
      </div>
    );
  } else if (selectedId) {
    body = (
      <div className="min-h-0 flex-1">
        <AgentView key={selectedId} id={selectedId} agent={selectedAgent} />
      </div>
    );
  } else {
    body = (
      <>
        {header}
        <div className="min-h-0 flex-1">
          <AgentList agents={agents} />
        </div>
      </>
    );
  }

  return (
    <div className="flex h-full flex-col pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)] pr-[env(safe-area-inset-right)]">
      <ConnectionBanner connected={connected} disabled={disabled} />
      {body}
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
