import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { StatusPill } from "./StatusPill";

function mm(reduced: boolean) {
  window.matchMedia = ((q: string) => ({
    matches: reduced && q.includes("prefers-reduced-motion"),
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia;
}
afterEach(() => mm(false));

describe("StatusPill", () => {
  it.each(["idle", "unknown"] as const)("renders nothing for %s", (s) => {
    mm(false);
    const { container } = render(<StatusPill status={s} />);
    expect(container).toBeEmptyDOMElement();
  });
  it("running shows RUNNING with a spinner", () => {
    mm(false);
    const { container } = render(<StatusPill status="running" />);
    expect(screen.getByText("RUNNING")).toBeInTheDocument();
    expect(container.querySelector("[data-spinner]")).toHaveClass("oo-spin");
  });
  it("waiting shows WAITING with a breathing dot", () => {
    mm(false);
    const { container } = render(<StatusPill status="waiting" />);
    expect(screen.getByText("WAITING")).toBeInTheDocument();
    expect(container.querySelector("[data-dot]")).toHaveClass("oo-breathe");
  });
  it("stopped shows STOPPED with an outline dot", () => {
    mm(false);
    const { container } = render(<StatusPill status="stopped" />);
    expect(screen.getByText("STOPPED")).toBeInTheDocument();
    expect(container.querySelector("[data-dot]")).not.toHaveClass("oo-breathe");
  });
  it("has no animation class under reduced motion", () => {
    mm(true);
    const a = render(<StatusPill status="running" />);
    expect(a.container.querySelector("[data-spinner]")).not.toHaveClass("oo-spin");
    a.unmount();
    const b = render(<StatusPill status="waiting" />);
    expect(b.container.querySelector("[data-dot]")).not.toHaveClass("oo-breathe");
  });
});
