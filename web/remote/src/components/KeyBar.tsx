import type { Key } from "@/lib/keys";
import { cn } from "@/lib/utils";

const KEYS: { key: Key; label: string; aria?: string }[] = [
  { key: "Esc", label: "Esc" },
  { key: "Tab", label: "Tab" },
  { key: "CtrlC", label: "^C", aria: "^C" },
  { key: "CtrlD", label: "^D", aria: "^D" },
  { key: "Up", label: "↑", aria: "Up" },
  { key: "Down", label: "↓", aria: "Down" },
  { key: "Left", label: "←", aria: "Left" },
  { key: "Right", label: "→", aria: "Right" },
  { key: "Enter", label: "⏎", aria: "Enter" },
];

const BTN =
  "inline-flex h-11 min-w-11 shrink-0 items-center justify-center rounded-md border bg-background px-3 text-sm font-medium select-none active:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

/** Keep focus in the terminal so the on-screen keyboard stays up. */
const keepFocus = (e: React.SyntheticEvent) => e.preventDefault();

export function KeyBar({
  ctrl,
  onToggleCtrl,
  onKey,
}: {
  ctrl: boolean;
  onToggleCtrl: () => void;
  onKey: (k: Key) => void;
}) {
  return (
    <div className="flex gap-1 overflow-x-auto px-2 py-1" role="toolbar" aria-label="Terminal keys">
      {KEYS.slice(0, 2).map((k) => (
        <button
          key={k.key}
          type="button"
          className={BTN}
          aria-label={k.aria ?? k.label}
          onMouseDown={keepFocus}
          onClick={() => onKey(k.key)}
        >
          {k.label}
        </button>
      ))}
      <button
        type="button"
        className={cn(BTN, ctrl && "bg-primary text-primary-foreground")}
        aria-pressed={ctrl}
        aria-label="Ctrl"
        onMouseDown={keepFocus}
        onClick={onToggleCtrl}
      >
        Ctrl
      </button>
      {KEYS.slice(2).map((k) => (
        <button
          key={k.key}
          type="button"
          className={BTN}
          aria-label={k.aria ?? k.label}
          onMouseDown={keepFocus}
          onClick={() => onKey(k.key)}
        >
          {k.label}
        </button>
      ))}
    </div>
  );
}
