import { PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Button } from "@/components/ui/button";
import { TopBar } from "./TopBar";

/**
 * Top bar over a collapsible list column and the detail pane. With no `list` (nothing shared)
 * there is no column, and no button to toggle it: the detail pane says it all.
 */
export function TabletLayout({ list, detail }: { list: ReactNode | null; detail: ReactNode }) {
  const [collapsed, setCollapsed] = useState(false);
  const showList = list !== null && !collapsed;
  return (
    <>
      <TopBar
        leading={
          list !== null && (
            <Button
              variant="ghost"
              size="icon"
              aria-label={collapsed ? "Show agent list" : "Hide agent list"}
              onClick={() => setCollapsed((c) => !c)}
            >
              {collapsed ? <PanelLeftOpen /> : <PanelLeftClose />}
            </Button>
          )
        }
      />
      <div className="flex min-h-0 flex-1">
        {showList && (
          <aside className="flex w-72 shrink-0 flex-col border-r" aria-label="Agents">
            <div className="min-h-0 flex-1">{list}</div>
          </aside>
        )}
        <section className="flex min-w-0 flex-1 flex-col" aria-label="Agent">
          <div className="min-h-0 flex-1">{detail}</div>
        </section>
      </div>
    </>
  );
}
