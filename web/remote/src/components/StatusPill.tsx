import type { AgentStatus } from "@/lib/api";
import { useReducedMotion } from "@/hooks/useReducedMotion";
import { cn } from "@/lib/utils";

/** Matches the desktop app's pills. Status is always spelled out in text, never color alone. */
export function StatusPill({ status, className }: { status: AgentStatus; className?: string }) {
  const reduced = useReducedMotion();
  if (status === "idle" || status === "unknown") return null;

  const base = "inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold tracking-wide";
  if (status === "running") {
    return (
      <span className={cn(base, "oo-pill-running", className)}>
        <span
          data-spinner
          aria-hidden="true"
          className={cn("size-2.5 rounded-full border-[1.5px] border-current border-t-transparent", !reduced && "oo-spin")}
        />
        RUNNING
      </span>
    );
  }
  if (status === "waiting") {
    return (
      <span className={cn(base, "oo-pill-waiting", className)}>
        <span data-dot aria-hidden="true" className={cn("size-1.5 rounded-full bg-current", !reduced && "oo-breathe")} />
        WAITING
      </span>
    );
  }
  return (
    <span className={cn(base, "oo-pill-stopped", className)}>
      <span data-dot aria-hidden="true" className="size-1.5 rounded-full border border-current" />
      STOPPED
    </span>
  );
}
