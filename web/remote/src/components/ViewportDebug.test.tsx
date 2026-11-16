import { describe, expect, it } from "vitest";
import { viewportDebugEnabled } from "./ViewportDebug";

describe("viewportDebugEnabled", () => {
  it("is off unless asked for", () => {
    expect(viewportDebugEnabled("", null)).toBe(false);
    expect(viewportDebugEnabled("?debug=other", null)).toBe(false);
  });
  it("turns on from the query and stays on for the session", () => {
    expect(viewportDebugEnabled("?debug=vv", null)).toBe(true);
    expect(viewportDebugEnabled("", "vv")).toBe(true);
  });
});
