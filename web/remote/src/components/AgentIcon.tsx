import claudecode from "@/assets/agents/claudecode.svg?url";
import codex from "@/assets/agents/codex.svg?url";
import cursor from "@/assets/agents/cursor.svg?url";
import kimi from "@/assets/agents/kimi.svg?url";
import opencode from "@/assets/agents/opencode.svg?url";
import pi from "@/assets/agents/pi.svg?url";

const ICONS: Record<string, { src: string; color: string }> = {
  "claude-code": { src: claudecode, color: "#d97757" },
  codex: { src: codex, color: "#10a37f" },
  kimi: { src: kimi, color: "#4f9cf9" },
  opencode: { src: opencode, color: "#3fb950" },
  pi: { src: pi, color: "#8b7cf6" },
  cursor: { src: cursor, color: "#9aa3af" },
  "cursor-agent": { src: cursor, color: "#9aa3af" },
};

/** Agent mark tinted with the desktop app's colours; unknown kinds get a monogram. */
export function AgentIcon({ kind, className = "" }: { kind: string | null; className?: string }) {
  const icon = kind ? ICONS[kind] : undefined;
  if (!icon) {
    return (
      <span
        data-agent={kind ?? ""}
        aria-hidden="true"
        className={`inline-flex size-4 shrink-0 items-center justify-center text-[14px] leading-none ${className}`}
        style={{ color: "#9aa3af" }}
      >
        ◇
      </span>
    );
  }
  const mask = `url("${icon.src}") center / contain no-repeat`;
  return (
    <span
      data-agent={kind ?? ""}
      role="img"
      aria-label={kind ?? undefined}
      className={`inline-block size-4 shrink-0 ${className}`}
      style={{ backgroundColor: icon.color, mask, WebkitMask: mask }}
    />
  );
}
