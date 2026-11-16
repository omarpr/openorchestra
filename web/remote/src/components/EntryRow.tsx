import { memo, useState } from "react";
import Markdown from "react-markdown";
import { Check, Loader2, X } from "lucide-react";
import type { ConvEntry } from "@/lib/conversation";

const GLYPHS: Record<string, string> = {
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

/** `{icon} {summary}`: a glyph for known tools (any case: pi's are lowercase), the name otherwise. */
function toolParts(name: string, summary: string): { glyph: string | null; text: string } {
  const glyph = GLYPHS[name.toLowerCase()] ?? null;
  const s = summary.trim();
  if (glyph) return { glyph, text: s || name };
  // Unknown tool: the name stands in for the glyph; never print it twice.
  return { glyph: name, text: s === name ? "" : s };
}

function Thinking({ e }: { e: ConvEntry }) {
  const [open, setOpen] = useState(false);
  const text = e.text?.trim() ?? "";
  const base = "flex min-h-11 w-full items-center text-left text-sm text-muted-foreground";
  if (!text) {
    const label =
      e.duration_ms != null ? `💭 Thought for ${Math.max(1, Math.round(e.duration_ms / 1000))} s` : "💭 Thinking (not recorded)";
    return <div className={base}>{label}</div>;
  }
  const first = text.split("\n")[0];
  return (
    <button type="button" className={`${base} ${open ? "items-start py-3" : ""}`} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
      {open ? (
        <span className="whitespace-pre-wrap break-words">
          💭 Thinking…{"\n"}
          {text}
        </span>
      ) : (
        <span className="min-w-0 truncate">
          💭 Thinking… <span className="italic">{first}</span>
        </span>
      )}
    </button>
  );
}

export const EntryRow = memo(function EntryRow({ e, onTool }: { e: ConvEntry; onTool(t: { id: string; name: string; summary: string }): void }) {
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
      return (
        <div className="max-w-full break-words text-sm leading-relaxed [&_a]:underline [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:font-mono [&_code]:text-[0.85em] [&_h1]:text-base [&_h1]:font-semibold [&_h2]:font-semibold [&_h3]:font-semibold [&_li]:ml-5 [&_ol]:list-decimal [&_p]:my-2 [&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-muted [&_pre]:p-2 [&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_ul]:list-disc">
          <Markdown
            disallowedElements={["img"]}
            components={{
              a: ({ node: _n, ...p }) => <a {...p} target="_blank" rel="noreferrer noopener" />,
            }}
          >
            {e.text ?? ""}
          </Markdown>
        </div>
      );
    case "thinking":
      return <Thinking e={e} />;
    case "tool": {
      const t = e.tool;
      if (!t) return null;
      const label = t.status === "ok" ? "✓" : t.status === "error" ? "✗" : "…";
      const { glyph, text } = toolParts(t.name, t.summary);
      const spoken = text && text !== t.name ? `${t.name} ${text}` : t.name;
      return (
        <button
          type="button"
          className="flex min-h-11 w-full items-center gap-2 rounded-md px-1 text-left font-mono text-xs text-muted-foreground hover:bg-muted"
          aria-label={`${label} ${spoken}`}
          onClick={() => onTool({ id: t.id, name: t.name, summary: t.summary })}
        >
          {t.status === "ok" && <Check aria-hidden className="size-3.5 shrink-0 text-green-600" />}
          {t.status === "error" && <X aria-hidden className="size-3.5 shrink-0 text-destructive" />}
          {t.status === "pending" && <Loader2 aria-hidden className="size-3.5 shrink-0 animate-spin" />}
          {glyph && <span className="max-w-[40%] shrink-0 truncate text-foreground">{glyph}</span>}
          {text && <span className="min-w-0 truncate">{text}</span>}
        </button>
      );
    }
    default:
      return <div className="py-1 text-center text-xs text-muted-foreground">{e.text}</div>;
  }
});
