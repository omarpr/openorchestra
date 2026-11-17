import { useState } from "react";
import { readPref, writePref } from "@/lib/storage";

export const MIN_FONT = 9;
export const MAX_FONT = 18;
const DEFAULT_FONT = 12;

/** Terminal view settings remembered on this device: fit (per agent), text size and the key bar. */
export function useTerminalPrefs(id: string) {
  const [fit, setFitState] = useState(() => readPref(`oo.fit.${id}`) === "1");
  const [fontSize, setFontSize] = useState(() => {
    const n = Number(readPref("oo.fontSize"));
    return n >= MIN_FONT && n <= MAX_FONT ? n : DEFAULT_FONT;
  });
  const [hideKeys, setHideKeys] = useState(() => readPref("oo.hideKeys") === "1");

  const setFit = (on: boolean) => {
    setFitState(on);
    writePref(`oo.fit.${id}`, on ? "1" : "0");
  };
  const stepFont = (delta: number) =>
    setFontSize((f) => {
      const n = Math.min(MAX_FONT, Math.max(MIN_FONT, f + delta));
      writePref("oo.fontSize", String(n));
      return n;
    });
  const toggleKeys = () =>
    setHideKeys((h) => {
      writePref("oo.hideKeys", h ? "0" : "1");
      return !h;
    });

  return { fit, setFit, fontSize, stepFont, hideKeys, toggleKeys };
}
