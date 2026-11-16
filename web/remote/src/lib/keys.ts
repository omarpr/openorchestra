export type Key = "Esc" | "Tab" | "CtrlC" | "CtrlD" | "Up" | "Down" | "Left" | "Right" | "Enter";

const BYTES: Record<Key, string> = {
  Esc: "\x1b",
  Tab: "\t",
  CtrlC: "\x03",
  CtrlD: "\x04",
  Up: "\x1b[A",
  Down: "\x1b[B",
  Right: "\x1b[C",
  Left: "\x1b[D",
  Enter: "\r",
};

export function keyBytes(k: Key): string {
  return BYTES[k];
}

/** Letter (or one of `@[\]^_?`) to its control character; anything else is unchanged. */
export function applyCtrl(ch: string): string {
  if (ch.length !== 1) return ch;
  if (ch === "?") return "\x7f";
  const c = ch.toUpperCase().charCodeAt(0);
  return c >= 0x40 && c <= 0x5f ? String.fromCharCode(c - 0x40) : ch;
}
