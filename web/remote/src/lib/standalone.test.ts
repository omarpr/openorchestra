import { afterEach, describe, expect, it, vi } from "vitest";
import { isIOS, isStandalone } from "./standalone";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
const IPAD = "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
const IPOD = "Mozilla/5.0 (iPod touch; CPU iPhone OS 15_0 like Mac OS X)";
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
const ANDROID = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/120 Mobile Safari/537.36";

afterEach(() => {
  vi.unstubAllGlobals();
});

function mm(matches: boolean) {
  window.matchMedia = ((q: string) => ({ matches, media: q })) as unknown as typeof window.matchMedia;
}

describe("isIOS", () => {
  it("detects iPhone, iPad, iPod UAs", () => {
    expect(isIOS(IPHONE, 5)).toBe(true);
    expect(isIOS(IPAD, 5)).toBe(true);
    expect(isIOS(IPOD, 5)).toBe(true);
  });
  it("detects iPadOS posing as Macintosh via touch points", () => {
    expect(isIOS(MAC, 5)).toBe(true);
    expect(isIOS(MAC, 0)).toBe(false);
    expect(isIOS(MAC, 1)).toBe(false);
  });
  it("is false for Android", () => {
    expect(isIOS(ANDROID, 5)).toBe(false);
  });
});

describe("isStandalone", () => {
  it("true for display-mode standalone", () => {
    mm(true);
    expect(isStandalone()).toBe(true);
  });
  it("true for navigator.standalone", () => {
    mm(false);
    Object.defineProperty(navigator, "standalone", { value: true, configurable: true });
    expect(isStandalone()).toBe(true);
    Object.defineProperty(navigator, "standalone", { value: undefined, configurable: true });
  });
  it("false in a plain browser tab", () => {
    mm(false);
    expect(isStandalone()).toBe(false);
  });
});
