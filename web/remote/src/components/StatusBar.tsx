import type { AgentStatus } from "@/lib/api";
import { useReducedMotion } from "@/hooks/useReducedMotion";
import { cn } from "@/lib/utils";

/** Live status strip under the conversation list. Idle/unknown hide it. */
export function StatusBar({ status, tool }: { status: AgentStatus; tool: string | null }) {
  const reduced = useReducedMotion();
  if (status === "idle" || status === "unknown") return null;

  const base = "flex shrink-0 items-center gap-2 border-t px-4 py-2 text-sm";
  if (status === "running") {
    return (
      <div role="status" aria-live="polite" className={cn(base, "text-[var(--oo-accent)]")}>
        {reduced ? (
          <span data-static-dot aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-current" />
        ) : (
          <span aria-hidden="true" className="flex shrink-0 items-center gap-1">
            {[0, 1, 2].map((i) => (
              <span
                key={i}
                data-bounce-dot
                className="oo-bounce size-1.5 rounded-full bg-current"
                style={{ animationDelay: `${i * 0.15}s` }}
              />
            ))}
          </span>
        )}
        <span className="font-medium">Working…</span>
        {tool && <span className="min-w-0 truncate text-muted-foreground">{tool}</span>}
      </div>
    );
  }
  if (status === "waiting") {
    return (
      <div role="status" aria-live="polite" className={cn(base, "oo-pill-waiting font-medium")}>
        <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-current" />
        Needs your input
      </div>
    );
  }
  return (
    <div role="status" aria-live="polite" className={cn(base, "text-muted-foreground")}>
      <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full border border-current" />
      Agent stopped
    </div>
  );
}
