import { describe, expect, it } from "vitest";
import { KEYBOARD_MIN_PX, shellVars, viewportBox } from "./useVisualViewport";

describe("viewportBox", () => {
  it("keyboard closed: full height at the top", () => {
    expect(viewportBox({ innerHeight: 844, height: 844, offsetTop: 0, scale: 1 })).toEqual({ height: 844, top: 0, keyboard: false });
  });

  it("iOS keyboard open: sized to the visible area, no lift", () => {
    expect(viewportBox({ innerHeight: 844, height: 508, offsetTop: 0, scale: 1 })).toEqual({ height: 508, top: 0, keyboard: true });
  });

  it("iOS keyboard open and panned: follows the visible area down the layout viewport", () => {
    const b = viewportBox({ innerHeight: 844, height: 508, offsetTop: 336, scale: 1 });
    expect(b).toEqual({ height: 508, top: 336, keyboard: true });
    // The shell's bottom edge is the keyboard's top edge: no gap.
    expect(b.top + b.height).toBe(844);
  });

  it("Android Chrome resizes the layout viewport: full height, nothing to compensate", () => {
    expect(viewportBox({ innerHeight: 520, height: 520, offsetTop: 0, scale: 1 })).toEqual({ height: 520, top: 0, keyboard: false });
  });

  it("toolbar-sized differences are not a keyboard", () => {
    expect(viewportBox({ innerHeight: 844, height: 844 - KEYBOARD_MIN_PX, offsetTop: 0, scale: 1 }).keyboard).toBe(false);
    expect(viewportBox({ innerHeight: 844, height: 844 - KEYBOARD_MIN_PX - 1, offsetTop: 0, scale: 1 }).keyboard).toBe(true);
  });

  it("pinch zoom: not mistaken for a keyboard, not pinned to the pan", () => {
    expect(viewportBox({ innerHeight: 844, height: 422, offsetTop: 200, scale: 2 })).toEqual({ height: 844, top: 0, keyboard: false });
  });

  it("zoomed with the keyboard open: sized in unzoomed px, top not pinned", () => {
    expect(viewportBox({ innerHeight: 844, height: 254, offsetTop: 120, scale: 2 })).toEqual({ height: 508, top: 0, keyboard: true });
  });

  it("rounds fractions and clamps rubber-band offsets", () => {
    expect(viewportBox({ innerHeight: 844, height: 507.6, offsetTop: -12.3, scale: 1 })).toEqual({ height: 508, top: 0, keyboard: true });
  });
});

describe("shellVars", () => {
  it("keyboard closed keeps the home-indicator inset", () => {
    expect(shellVars({ height: 844, top: 0, keyboard: false })).toEqual({
      "--oo-vv-height": "844px",
      "--oo-vv-top": "0px",
      "--oo-safe-bottom": "env(safe-area-inset-bottom)",
    });
  });

  it("keyboard open drops the inset (the keyboard covers the home indicator)", () => {
    expect(shellVars({ height: 508, top: 336, keyboard: true })).toEqual({
      "--oo-vv-height": "508px",
      "--oo-vv-top": "336px",
      "--oo-safe-bottom": "0px",
    });
  });
});
