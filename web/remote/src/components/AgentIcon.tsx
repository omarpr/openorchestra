import { SquareTerminal } from "lucide-react";
import claudecode from "@/assets/agents/claudecode.svg?url";
import codex from "@/assets/agents/codex.svg?url";
import cursor from "@/assets/agents/cursor.svg?url";
import grok from "@/assets/agents/grok.svg?url";
import kimi from "@/assets/agents/kimi.svg?url";
import opencode from "@/assets/agents/opencode.svg?url";
import pi from "@/assets/agents/pi.svg?url";

const ICONS: Record<string, { src: string; color: string; maskSize?: string }> = {
  "claude-code": { src: claudecode, color: "#d97757" },
  codex: { src: codex, color: "#10a37f" },
  grok: { src: grok, color: "#e6e6e6" },
  kimi: { src: kimi, color: "#4f9cf9" },
  opencode: { src: opencode, color: "#3fb950", maskSize: "1.125rem" },
  pi: { src: pi, color: "#8b7cf6", maskSize: "1rem" },
  cursor: { src: cursor, color: "#9aa3af" },
  "cursor-agent": { src: cursor, color: "#9aa3af" },
};

/** Agent mark tinted with the desktop app's colours; every mark takes the same space. */
export function AgentIcon({ kind, className = "" }: { kind: string | null; className?: string }) {
  const box = `grid size-6 shrink-0 place-items-center ${className}`;
  if (kind === null) {
    return (
      <span data-agent="shell" aria-hidden="true" className={box}>
        <SquareTerminal className="size-[1.125rem] text-muted-foreground" />
      </span>
    );
  }
  const icon = ICONS[kind];
  if (!icon) {
    return (
      <span
        data-agent={kind}
        aria-hidden="true"
        className={`${box} text-base leading-none`}
        style={{ color: "#9aa3af" }}
      >
        ◇
      </span>
    );
  }
  // The source SVGs share a viewBox, but some have more padding in the artwork.
  const mask = `url("${icon.src}") center / ${icon.maskSize ?? "0.9375rem"} no-repeat`;
  return (
    <span data-agent={kind} role="img" aria-label={kind} className={box}>
      <span className="size-5" style={{ backgroundColor: icon.color, mask, WebkitMask: mask }} />
    </span>
  );
}
