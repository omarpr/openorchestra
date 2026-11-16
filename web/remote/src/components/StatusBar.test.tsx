import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { StatusBar } from "./StatusBar";

function mm(reduced: boolean) {
  window.matchMedia = ((q: string) => ({
    matches: reduced && q.includes("prefers-reduced-motion"),
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia;
}
afterEach(() => mm(false));

describe("StatusBar", () => {
  it("running shows Working… with three bouncing dots and the tool", () => {
    mm(false);
    const { container } = render(<StatusBar status="running" tool="Bash: ls" />);
    const bar = screen.getByRole("status");
    expect(bar).toHaveAttribute("aria-live", "polite");
    expect(bar).toHaveTextContent("Working…");
    expect(bar).toHaveTextContent("Bash: ls");
    const dots = container.querySelectorAll("[data-bounce-dot]");
    expect(dots).toHaveLength(3);
    dots.forEach((d) => expect(d).toHaveClass("oo-bounce"));
  });

  it("running without a tool omits the summary", () => {
    mm(false);
    render(<StatusBar status="running" tool={null} />);
    expect(screen.getByRole("status")).toHaveTextContent(/^Working…$/);
  });

  it("reduced motion swaps the animation for a static dot", () => {
    mm(true);
    const { container } = render(<StatusBar status="running" tool={null} />);
    expect(container.querySelector("[data-bounce-dot]")).toBeNull();
    expect(container.querySelector("[data-static-dot]")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Working…");
  });

  it("waiting shows Needs your input in amber", () => {
    render(<StatusBar status="waiting" tool={null} />);
    const bar = screen.getByRole("status");
    expect(bar).toHaveTextContent("Needs your input");
    expect(bar).toHaveClass("oo-pill-waiting");
  });

  it("stopped shows Agent stopped", () => {
    render(<StatusBar status="stopped" tool={null} />);
    expect(screen.getByRole("status")).toHaveTextContent("Agent stopped");
  });

  it.each(["idle", "unknown"] as const)("%s renders nothing", (s) => {
    const { container } = render(<StatusBar status={s} tool={null} />);
    expect(container).toBeEmptyDOMElement();
  });
});
