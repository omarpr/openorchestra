import { describe, expect, it } from "vitest";
import { applyCtrl, keyBytes } from "./keys";

describe("keys", () => {
  it("maps named keys to terminal bytes", () => {
    expect(keyBytes("Esc")).toBe("\x1b");
    expect(keyBytes("Tab")).toBe("\t");
    expect(keyBytes("CtrlC")).toBe("\x03");
    expect(keyBytes("CtrlD")).toBe("\x04");
    expect(keyBytes("Up")).toBe("\x1b[A");
    expect(keyBytes("Down")).toBe("\x1b[B");
    expect(keyBytes("Right")).toBe("\x1b[C");
    expect(keyBytes("Left")).toBe("\x1b[D");
    expect(keyBytes("Enter")).toBe("\r");
  });

  it("applyCtrl turns letters into control chars", () => {
    expect(applyCtrl("a")).toBe("\x01");
    expect(applyCtrl("Z")).toBe("\x1a");
    expect(applyCtrl("1")).toBe("1");
  });
});
