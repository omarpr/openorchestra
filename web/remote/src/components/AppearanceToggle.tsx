import { Monitor, Moon, Sun } from "lucide-react";
import { useSyncExternalStore } from "react";
import { type Appearance, getAppearance, setAppearance, subscribeAppearance } from "@/lib/appearance";
import { cn } from "@/lib/utils";

const OPTIONS: { value: Appearance; label: string; Icon: typeof Sun }[] = [
  { value: "system", label: "System", Icon: Monitor },
  { value: "light", label: "Light", Icon: Sun },
  { value: "dark", label: "Dark", Icon: Moon },
];

/** System / Light / Dark, as a compact segmented control (one pressed button). */
export function AppearanceToggle({ className }: { className?: string }) {
  const current = useSyncExternalStore(subscribeAppearance, getAppearance);
  return (
    <fieldset aria-label="Appearance" className={cn("m-0 flex min-w-0 shrink-0 rounded-lg border p-0.5", className)}>
      {OPTIONS.map(({ value, label, Icon }) => (
        <button
          key={value}
          type="button"
          aria-pressed={current === value}
          aria-label={label}
          title={label}
          onClick={() => setAppearance(value)}
          className={cn(
            "flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            current === value ? "bg-accent text-foreground" : "hover:text-foreground",
          )}
        >
          <Icon className="size-4" />
        </button>
      ))}
    </fieldset>
  );
}
