import { useState, useSyncExternalStore } from "react";
import { PanelLeftClose, PanelLeftOpen, ChevronLeft } from "lucide-react";
import { useAgents } from "@/hooks/useAgents";
import { useIsTablet } from "@/hooks/useMedia";
import { getToken, subscribeToken } from "@/lib/token";
import { navigate, useRoute } from "@/lib/router";
import { AgentList } from "@/components/AgentList";
import { AgentView } from "@/components/AgentView";
import { ConnectionBanner } from "@/components/ConnectionBanner";
import { PairScreen } from "@/components/PairScreen";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";

function Main() {
  const { agents, connected, disabled } = useAgents();
  const isTablet = useIsTablet();
  const route = useRoute();
  const [collapsed, setCollapsed] = useState(false);
  const selectedId = route.name === "agent" ? route.id : undefined;

  const header = (
    <header className="flex h-14 shrink-0 items-center justify-between border-b px-4">
      <h1 className="font-semibold">Agents</h1>
    </header>
  );

  let body;
  if (isTablet) {
    body = (
      <div className="flex min-h-0 flex-1">
        {!collapsed && (
          <aside className="flex w-80 shrink-0 flex-col border-r" aria-label="Agents">
            <div className="flex h-14 shrink-0 items-center justify-between border-b pl-4 pr-1">
              <h1 className="font-semibold">Agents</h1>
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
              <AgentView key={selectedId} id={selectedId} />
            ) : (
              <div className="flex h-full items-center justify-center text-muted-foreground">Select an agent</div>
            )}
          </div>
        </section>
      </div>
    );
  } else if (selectedId) {
    body = (
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="flex h-14 shrink-0 items-center border-b px-1">
          <Button variant="ghost" size="icon" aria-label="Back to agents" onClick={() => navigate("#/")}>
            <ChevronLeft />
          </Button>
        </div>
        <div className="min-h-0 flex-1">
          <AgentView key={selectedId} id={selectedId} />
        </div>
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
  return (
    <>
      {token ? <Main /> : <PairScreen />}
      <Toaster position="top-center" />
    </>
  );
}
