import { useEffect, useState } from "react";

/**
 * Hidden diagnostics for the iOS keyboard gap: open the app with `?debug=vv` (kept for the session).
 * Shows the browser's own viewport numbers so a screenshot tells where the extra space comes from.
 */
export function viewportDebugEnabled(search: string, stored: string | null): boolean {
  return new URLSearchParams(search).get("debug") === "vv" || stored === "vv";
}

function safeAreaInsets(): string {
  const probe = document.createElement("div");
  probe.style.cssText =
    "position:fixed;visibility:hidden;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left)";
  document.body.appendChild(probe);
  const cs = getComputedStyle(probe);
  const out = `${cs.paddingTop}/${cs.paddingRight}/${cs.paddingBottom}/${cs.paddingLeft}`;
  probe.remove();
  return out;
}

function snapshot(): string[] {
  const vv = window.visualViewport;
  const shell = document.querySelector(".oo-shell") as HTMLElement | null;
  const sr = shell?.getBoundingClientRect();
  const active = document.activeElement as HTMLElement | null;
  const ar = active && active !== document.body ? active.getBoundingClientRect() : null;
  const vvBottom = vv ? vv.offsetTop + vv.height : NaN;
  const standalone =
    window.matchMedia?.("(display-mode: standalone)").matches || (navigator as { standalone?: boolean }).standalone === true;
  const r = (n: number | undefined) => (n === undefined || Number.isNaN(n) ? "–" : Math.round(n));
  return [
    `standalone ${standalone}  ua ${/CriOS/.test(navigator.userAgent) ? "Chrome" : /Safari/.test(navigator.userAgent) ? "Safari" : "other"}`,
    `inner ${r(window.innerHeight)} outer ${r(window.outerHeight)} docEl ${r(document.documentElement.clientHeight)}`,
    `vv h ${r(vv?.height)} top ${r(vv?.offsetTop)} pageTop ${r(vv?.pageTop)} scale ${vv?.scale.toFixed(2) ?? "–"}`,
    `scrollY ${r(window.scrollY)}  body h ${r(document.body.scrollHeight)}`,
    `shell top ${r(sr?.top)} bottom ${r(sr?.bottom)} h ${r(sr?.height)}  vars ${shell?.style.getPropertyValue("--oo-vv-height") || "–"} / ${shell?.style.getPropertyValue("--oo-vv-top") || "–"} / ${shell?.style.getPropertyValue("--oo-safe-bottom") || "–"}`,
    `pad-bottom ${shell ? getComputedStyle(shell).paddingBottom : "–"}  safe ${safeAreaInsets()}`,
    `focused ${active?.tagName.toLowerCase() ?? "–"} bottom ${r(ar?.bottom)}  gap→vv-bottom ${ar ? r(vvBottom - ar.bottom) : "–"}`,
  ];
}

export function ViewportDebug() {
  const [on] = useState(() => {
    let stored: string | null = null;
    try {
      stored = sessionStorage.getItem("oo.debug");
    } catch {
      /* storage unavailable */
    }
    const enabled = viewportDebugEnabled(window.location.search, stored);
    if (enabled) {
      try {
        sessionStorage.setItem("oo.debug", "vv");
      } catch {
        /* storage unavailable */
      }
    }
    return enabled;
  });
  const [lines, setLines] = useState<string[]>([]);
  useEffect(() => {
    if (!on) return;
    const update = () => setLines(snapshot());
    update();
    const id = window.setInterval(update, 250);
    return () => window.clearInterval(id);
  }, [on]);
  if (!on) return null;
  return (
    <pre
      aria-hidden
      className="pointer-events-none fixed left-1 right-1 top-[env(safe-area-inset-top)] z-[9999] m-0 whitespace-pre-wrap rounded bg-black/80 p-1 font-mono text-[10px] leading-tight text-lime-300"
    >
      {lines.join("\n")}
    </pre>
  );
}
