import type { AgentStatus } from "@/lib/api";
import { cn } from "@/lib/utils";

const STYLES: Record<AgentStatus, string> = {
  running: "bg-primary text-primary-foreground border-transparent",
  waiting: "bg-amber-400 text-amber-950 border-transparent animate-pulse motion-reduce:animate-none",
  idle: "bg-muted text-muted-foreground border-transparent",
  unknown: "bg-muted text-muted-foreground border-transparent",
  stopped: "border-destructive text-destructive bg-transparent",
};

/** Status is always spelled out in text, never color alone. */
export function StatusPill({ status, className }: { status: AgentStatus; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wide",
        STYLES[status],
        className,
      )}
    >
      {status}
    </span>
  );
}
