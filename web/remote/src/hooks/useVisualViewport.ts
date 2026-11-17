import { type RefObject, useLayoutEffect } from "react";

/** Below this much hidden height the difference is browser chrome, not the on-screen keyboard. */
export const KEYBOARD_MIN_PX = 100;
const ZOOM_EPSILON = 0.01;

export interface ViewportInput {
  /** window.innerHeight: the layout viewport, which iOS WebKit does not shrink for the keyboard. */
  innerHeight: number;
  /** visualViewport.height, in CSS px at the current zoom. */
  height: number;
  /** visualViewport.offsetTop: how far iOS panned the visible area down the layout viewport. */
  offsetTop: number;
  /** visualViewport.scale (pinch or focus zoom). */
  scale: number;
}

export interface ViewportBox {
  /** Height of the app shell, in px: the visible area. */
  height: number;
  /** Top of the app shell inside the layout viewport, in px. */
  top: number;
  /** The on-screen keyboard covers the bottom of the layout viewport. */
  keyboard: boolean;
}

/**
 * Where the app shell goes so it covers exactly what the user can see.
 * iOS keeps the layout viewport full height under the keyboard and pans the visible area instead;
 * Android Chrome shrinks the layout viewport, so there height === innerHeight and top === 0.
 */
export function viewportBox(i: ViewportInput): ViewportBox {
  const zoomed = i.scale > 1 + ZOOM_EPSILON;
  const visible = i.height * (zoomed ? i.scale : 1);
  const hidden = i.innerHeight - visible;
  return {
    height: Math.max(0, Math.round(Math.min(i.innerHeight, visible))),
    // While zoomed the user pans freely; pinning would fight them.
    top: zoomed ? 0 : Math.max(0, Math.round(i.offsetTop)),
    keyboard: hidden > KEYBOARD_MIN_PX,
  };
}

/** CSS variables the shell reads (see .oo-shell in index.css). */
export function shellVars(b: ViewportBox): Record<string, string> {
  return {
    "--oo-vv-height": `${b.height}px`,
    "--oo-vv-top": `${b.top}px`,
    // The keyboard covers the home indicator, so its inset would only add a gap above the keyboard.
    "--oo-safe-bottom": b.keyboard ? "0px" : "env(safe-area-inset-bottom)",
  };
}

/**
 * Pin the element to the visual viewport: sized to the visible area and moved with iOS's pan, written
 * straight to the element's style (no React re-render per viewport event). Without visualViewport the
 * element keeps its stylesheet layout (height: 100%).
 */
export function useVisualViewport(ref: RefObject<HTMLElement | null>): void {
  useLayoutEffect(() => {
    const vv = window.visualViewport;
    const el = ref.current;
    if (!vv || !el) return;
    const update = () => {
      const box = viewportBox({
        innerHeight: window.innerHeight,
        height: vv.height,
        offsetTop: vv.offsetTop,
        scale: vv.scale,
      });
      for (const [k, v] of Object.entries(shellVars(box))) el.style.setProperty(k, v);
      // iOS may also scroll the document to reveal the input; the shell is fixed, so that scroll only
      // shifts things. Undo it (keyboard open, not zoomed, only when actually scrolled).
      if (box.keyboard && vv.scale <= 1 + ZOOM_EPSILON && window.scrollY !== 0) window.scrollTo(0, 0);
    };
    el.dataset.vv = "";
    update();
    vv.addEventListener("resize", update);
    vv.addEventListener("scroll", update);
    window.addEventListener("resize", update);
    return () => {
      vv.removeEventListener("resize", update);
      vv.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      delete el.dataset.vv;
      for (const k of Object.keys(shellVars({ height: 0, top: 0, keyboard: false }))) el.style.removeProperty(k);
    };
  }, [ref]);
}
