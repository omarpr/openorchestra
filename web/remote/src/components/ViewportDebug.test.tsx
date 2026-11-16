import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ViewportDebug, viewportDebugEnabled } from "./ViewportDebug";

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

describe("ViewportDebug", () => {
  afterEach(() => {
    cleanup();
    sessionStorage.clear();
  });
  it("shows the shell box and dashes when the shell sets no viewport variables", () => {
    sessionStorage.setItem("oo.debug", "vv");
    render(
      <div className="oo-shell">
        <ViewportDebug />
      </div>,
    );
    const text = screen.getByText(/shell top/).textContent ?? "";
    expect(text).toMatch(/shell top 0 bottom 0 h 0/);
    expect(text).toContain("vars – / – / –");
  });
});
