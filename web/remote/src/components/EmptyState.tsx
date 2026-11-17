import type { ReactNode } from "react";
import { RemoteIllustration } from "@/components/illustrations/RemoteIllustration";
import { cn } from "@/lib/utils";

/** A centered illustration, title, explanation and optional action, for screens with nothing to show. */
export function EmptyState({
  title,
  children,
  action,
  compact = false,
  className,
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  /** Smaller art, for narrow places like the agent list. */
  compact?: boolean;
  className?: string;
}) {
  return (
    <div className={cn("flex h-full flex-col items-center justify-center gap-3 p-6 text-center", className)}>
      <RemoteIllustration className={compact ? "w-36" : "w-56 max-w-[60%]"} />
      <h2 className={cn("font-semibold", compact ? "text-base" : "text-lg")}>{title}</h2>
      {children && <p className="max-w-sm text-sm text-muted-foreground">{children}</p>}
      {action}
    </div>
  );
}
