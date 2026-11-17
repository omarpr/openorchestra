import { Keyboard } from "lucide-react";
import type { RefObject } from "react";
import { KeyBar } from "@/components/KeyBar";
import { QuickReply } from "@/components/QuickReply";
import { Button } from "@/components/ui/button";
import type { Key } from "@/lib/keys";

/** The bottom bar: reply box, plus the key bar and its toggle in the terminal view. */
export function AgentComposer({
  id,
  name,
  stopped,
  inputRef,
  terminal,
}: {
  id: string;
  /** What the tab runs ("Claude Code"); `null` for a shell. */
  name: string | null;
  stopped: boolean;
  inputRef: RefObject<HTMLInputElement>;
  /** Present in the terminal view. */
  terminal?: { hideKeys: boolean; toggleKeys(): void; ctrl: boolean; toggleCtrl(): void; sendKey(k: Key): void };
}) {
  return (
    <div className="shrink-0 border-t bg-background">
      {terminal && !terminal.hideKeys && (
        <KeyBar ctrl={terminal.ctrl} onToggleCtrl={terminal.toggleCtrl} onKey={terminal.sendKey} />
      )}
      <div className="flex items-center gap-2 px-2 pb-2">
        <QuickReply id={id} disabled={stopped} name={name} inputRef={inputRef} />
        {terminal && (
          <Button
            variant="outline"
            size="icon"
            aria-label={terminal.hideKeys ? "Show keys" : "Hide keys"}
            aria-pressed={!terminal.hideKeys}
            onClick={terminal.toggleKeys}
          >
            <Keyboard />
          </Button>
        )}
      </div>
    </div>
  );
}
