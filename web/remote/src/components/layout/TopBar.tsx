import type { ReactNode } from "react";
import { AppearanceToggle } from "@/components/AppearanceToggle";
import { cn } from "@/lib/utils";
import { Brand } from "./Brand";

/** The full-width bar: `leading` (e.g. the list toggle) and the brand on the left, appearance on the right. */
export function TopBar({ leading, className }: { leading?: ReactNode; className?: string }) {
  return (
    <header className={cn("flex h-14 shrink-0 items-center gap-2 border-b pr-4", leading ? "pl-1" : "pl-4", className)}>
      {leading}
      <Brand />
      <AppearanceToggle className="ml-auto" />
    </header>
  );
}
