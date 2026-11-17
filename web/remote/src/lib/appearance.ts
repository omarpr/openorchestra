// Appearance: follow the system, or force light / dark. Stored per device; applied as
// `data-theme` on <html>, which the CSS and the `dark:` variant read.
import { readPref, writePref } from "./storage";

export type Appearance = "system" | "light" | "dark";

const KEY = "oo.appearance";
const QUERY = "(prefers-color-scheme: dark)";
const listeners = new Set<() => void>();

export function getAppearance(): Appearance {
  const v = readPref(KEY);
  return v === "light" || v === "dark" ? v : "system";
}

function systemDark(): boolean {
  return typeof window.matchMedia === "function" ? window.matchMedia(QUERY).matches : false;
}

/** Whether the app is drawn dark right now. */
export function isDark(): boolean {
  const a = getAppearance();
  return a === "dark" || (a === "system" && systemDark());
}

export function applyAppearance(): void {
  const a = getAppearance();
  const root = document.documentElement;
  if (a === "system") delete root.dataset.theme;
  else root.dataset.theme = a;
  // The browser/status bar color: one tag, set to what is shown.
  const color = isDark() ? "#0a0a0a" : "#ffffff";
  document.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]').forEach((m) => {
    m.content = color;
    m.removeAttribute("media");
  });
}

export function setAppearance(a: Appearance): void {
  writePref(KEY, a === "system" ? null : a);
  applyAppearance();
  for (const l of listeners) l();
}

/** Notified on a choice here or a system light/dark switch. For useSyncExternalStore. */
export function subscribeAppearance(cb: () => void): () => void {
  listeners.add(cb);
  const mq = typeof window.matchMedia === "function" ? window.matchMedia(QUERY) : null;
  const onSystem = () => {
    applyAppearance();
    cb();
  };
  mq?.addEventListener?.("change", onSystem);
  return () => {
    listeners.delete(cb);
    mq?.removeEventListener?.("change", onSystem);
  };
}
