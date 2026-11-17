import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const TONES = {
  warning: "bg-amber-100 text-amber-950 dark:bg-amber-950 dark:text-amber-100",
  muted: "bg-muted text-muted-foreground",
};

/** A one-line status strip, with a Retry button when there is something to retry. */
export function StatusBanner({
  tone,
  children,
  onRetry,
  className,
}: {
  tone: keyof typeof TONES;
  children: ReactNode;
  onRetry?: () => void;
  className?: string;
}) {
  return (
    <div role="status" className={cn("flex shrink-0 items-center gap-3 px-4 py-1.5 text-sm", TONES[tone], className)}>
      <span className="min-w-0 flex-1">{children}</span>
      {onRetry && (
        <Button variant="outline" size="sm" className="h-7 shrink-0 bg-transparent" onClick={onRetry}>
          Retry
        </Button>
      )}
    </div>
  );
}
