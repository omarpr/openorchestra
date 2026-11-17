import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { MAX_FONT, MIN_FONT } from "@/hooks/useTerminalPrefs";
import type { View } from "@/hooks/useViewChoice";

const TRIGGER = "h-7 min-h-0 px-2.5 text-xs";

/** Conversation/Terminal switch, terminal text size, and the watch-only badge. */
export function AgentToolbar({
  view,
  onPickView,
  canPick,
  fontSize,
  onStepFont,
  watchOnly,
}: {
  view: View;
  onPickView(v: View): void;
  /** Whether there is a conversation to switch to. */
  canPick: boolean;
  fontSize: number;
  onStepFont(delta: number): void;
  watchOnly: boolean;
}) {
  return (
    <div className="flex min-h-11 shrink-0 items-center gap-1 border-b px-2 text-sm">
      {canPick && (
        <Tabs value={view} onValueChange={(v) => onPickView(v as View)}>
          <TabsList className="h-8 p-0.5">
            <TabsTrigger value="conversation" className={TRIGGER}>
              Conversation
            </TabsTrigger>
            <TabsTrigger value="terminal" className={TRIGGER}>
              Terminal
            </TabsTrigger>
          </TabsList>
        </Tabs>
      )}
      {view === "terminal" && (
        <>
          <Button
            variant="ghost"
            size="icon"
            aria-label="Smaller text"
            disabled={fontSize <= MIN_FONT}
            onClick={() => onStepFont(-1)}
          >
            A−
          </Button>
          <span className="w-8 text-center tabular-nums text-muted-foreground" aria-live="polite">
            {fontSize}
          </span>
          <Button
            variant="ghost"
            size="icon"
            aria-label="Larger text"
            disabled={fontSize >= MAX_FONT}
            onClick={() => onStepFont(1)}
          >
            A+
          </Button>
        </>
      )}
      <div className="flex-1" />
      {watchOnly && <Badge variant="secondary">Watch only</Badge>}
    </div>
  );
}
