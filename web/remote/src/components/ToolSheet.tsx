import { useEffect, useState } from "react";
import { fetchToolResult, type ToolResult } from "@/lib/api";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";

export interface ToolRef {
  id: string;
  name: string;
  summary: string;
}

/** Bottom sheet showing one tool call's output, fetched on open. */
export function ToolSheet({ agentId, tool, onClose }: { agentId: string; tool: ToolRef | null; onClose(): void }) {
  const [res, setRes] = useState<ToolResult | null>(null);
  const [failed, setFailed] = useState(false);
  const toolId = tool?.id;

  useEffect(() => {
    if (!toolId) return;
    let live = true;
    setRes(null);
    setFailed(false);
    fetchToolResult(agentId, toolId)
      .then((r) => live && setRes(r))
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [agentId, toolId]);

  return (
    <Sheet open={!!tool} onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="bottom" className="flex max-h-[85dvh] flex-col p-4">
        <SheetHeader className="pr-10">
          <SheetTitle className="truncate text-base">{tool?.name}</SheetTitle>
          <SheetDescription className="truncate font-mono">{tool?.summary}</SheetDescription>
        </SheetHeader>
        <div className="mt-3 min-h-0 flex-1 overflow-auto rounded-md bg-muted p-3">
          {failed ? (
            <p className="text-sm text-muted-foreground">Output isn't available</p>
          ) : res === null ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : (
            <>
              <pre className={`whitespace-pre-wrap break-words font-mono text-xs ${res.is_error ? "text-destructive" : ""}`}>
                {res.text || "(no output)"}
              </pre>
              {res.truncated && <p className="mt-2 font-mono text-xs text-muted-foreground">…[truncated]</p>}
            </>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
