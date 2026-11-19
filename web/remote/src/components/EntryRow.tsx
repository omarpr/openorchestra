import { Check, Loader2, X } from "lucide-react";
import { memo, useState } from "react";
import { type ConvEntry, isThinkingMarker } from "@/lib/conversation";
import { MessageMarkdown } from "./MessageMarkdown";
import type { ToolRef } from "./ToolSheet";

const GLYPHS: Record<string, string> = {
  $: "$",
  edit: "✎",
  write: "✎",
  multiedit: "✎",
  notebookedit: "✎",
  read: "📄",
  bash: "$",
  grep: "🔍",
  glob: "🔍",
  webfetch: "🌐",
  websearch: "🌐",
  task: "🤖",
  agent: "🤖",
  todowrite: "☑",
};

const TRANSCRIPT_ROW =
  "grid min-h-11 w-full grid-cols-[0.875rem_1.25rem_minmax(0,1fr)] items-center gap-x-2 text-left text-muted-foreground";
const GLYPH = "flex items-center justify-center leading-none text-foreground";

function Thinking({ e, live }: { e: ConvEntry; live: boolean }) {
  const [open, setOpen] = useState(false);
  const text = e.text?.trim() ?? "";
  const base = `${TRANSCRIPT_ROW} text-sm`;
  if (!text) {
    const label = live ? "Thinking…" : `Thought for ${Math.max(1, Math.round((e.duration_ms ?? 0) / 1000))} s`;
    return (
      <div className={base} role={live ? "status" : undefined}>
        <span aria-hidden="true" className={`${GLYPH} col-start-2`}>
          {live ? (
            <span className="relative h-1.5 w-5 rounded-full bg-current/15 text-[var(--oo-accent)]">
              <span className="oo-thinking-dot absolute top-0 left-0 size-1.5 rounded-full bg-current" />
            </span>
          ) : (
            "💭"
          )}
        </span>
        <span>{label}</span>
      </div>
    );
  }
  const first = text.split("\n")[0];
  return (
    <button
      type="button"
      className={`${base} ${open ? "items-start py-3" : ""}`}
      aria-expanded={open}
      onClick={() => setOpen((o) => !o)}
    >
      <span aria-hidden="true" className={`${GLYPH} col-start-2`}>
        💭
      </span>
      {open ? (
        <span className="whitespace-pre-wrap break-words">
          Thinking…{"\n"}
          {text}
        </span>
      ) : (
        <span className="min-w-0 truncate">
          Thinking… <span className="italic">{first}</span>
        </span>
      )}
    </button>
  );
}

function ToolRow({ tool: t, onTool }: { tool: NonNullable<ConvEntry["tool"]>; onTool: (tool: ToolRef) => void }) {
  const label = t.status === "ok" ? "✓" : t.status === "error" ? "✗" : "…";
  const glyph = GLYPHS[t.name.toLowerCase()];
  const summary = t.summary.trim();
  const detail = summary === t.name ? "" : summary;
  const spoken = detail ? `${t.name} ${detail}` : t.name;
  return (
    <button
      type="button"
      className={`${TRANSCRIPT_ROW} rounded-md font-mono text-xs hover:bg-muted`}
      aria-label={`${label} ${spoken}`}
      onClick={() => onTool(t)}
    >
      {t.status === "ok" && <Check aria-hidden className="size-3.5 shrink-0 text-green-600" />}
      {t.status === "error" && <X aria-hidden className="size-3.5 shrink-0 text-destructive" />}
      {t.status === "pending" && <Loader2 aria-hidden className="size-3.5 shrink-0 animate-spin" />}
      <span aria-hidden="true" className={GLYPH}>
        {glyph}
      </span>
      <span className="flex min-w-0 items-baseline gap-2">
        {glyph ? (
          <span className="truncate">{detail || t.name}</span>
        ) : (
          <>
            <span className={detail ? "max-w-[40%] shrink-0 truncate text-foreground" : "truncate text-foreground"}>
              {t.name}
            </span>
            {detail && <span className="min-w-0 truncate">{detail}</span>}
          </>
        )}
      </span>
    </button>
  );
}

export const EntryRow = memo(function EntryRow({
  e,
  onTool,
  liveThinking,
}: {
  e: ConvEntry;
  onTool(t: ToolRef): void;
  liveThinking: boolean;
}) {
  switch (e.kind) {
    case "user":
      return (
        <div className="flex justify-end">
          <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-sm bg-primary px-3 py-2 text-sm text-primary-foreground">
            {e.text}
          </div>
        </div>
      );
    case "assistant":
      return <MessageMarkdown text={e.text ?? ""} />;
    case "thinking":
      return liveThinking || !isThinkingMarker(e) ? <Thinking e={e} live={liveThinking} /> : null;
    case "tool":
      return e.tool ? <ToolRow tool={e.tool} onTool={onTool} /> : null;
    default:
      return <div className="py-1 text-center text-xs text-muted-foreground">{e.text}</div>;
  }
});
