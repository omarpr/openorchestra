import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/hooks/useAgents", () => ({
  useAgents: () => ({ agents: [], connected: true, unauthorized: false, disabled: false }),
}));

import App from "./App";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
const ANDROID = "Mozilla/5.0 (Linux; Android 14; Pixel 8) Mobile Safari/537.36";

function setup(ua: string, standalone: boolean, tablet = false) {
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue(ua);
  window.matchMedia = ((q: string) => ({
    matches: q.includes("display-mode") ? standalone : q.includes("min-width") ? tablet : false,
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia;
}

beforeEach(() => localStorage.clear());
afterEach(async () => {
  vi.restoreAllMocks();
  await new Promise((r) => setTimeout(r, 150)); // input-otp timers
});

describe("pair gate", () => {
  it("shows InstallFirst on iOS in a browser tab", () => {
    setup(IPHONE, false);
    render(<App />);
    expect(screen.getByText("Add openOrchestra to your Home Screen")).toBeInTheDocument();
    expect(screen.getByText("Tap Share")).toBeInTheDocument();
    expect(screen.getByText("Choose Add to Home Screen")).toBeInTheDocument();
    expect(screen.getByText("Open openOrchestra from your Home Screen and pair there")).toBeInTheDocument();
    expect(screen.queryByLabelText("Pairing code")).not.toBeInTheDocument();
  });
  it("shows the pair form on Android", () => {
    setup(ANDROID, false);
    render(<App />);
    expect(screen.getByLabelText("Pairing code")).toBeInTheDocument();
    expect(screen.queryByText("Add openOrchestra to your Home Screen")).not.toBeInTheDocument();
  });
  it("shows the pair form on iOS when installed", () => {
    setup(IPHONE, true);
    render(<App />);
    expect(screen.getByLabelText("Pairing code")).toBeInTheDocument();
  });
  it("reveals the pair form via the escape link", async () => {
    setup(IPHONE, false);
    render(<App />);
    await userEvent.click(screen.getByText("Pair in this browser instead"));
    expect(screen.getByLabelText("Pairing code")).toBeInTheDocument();
  });
});

describe("header", () => {
  it("reads openOrchestra Remote on phone", () => {
    setup(ANDROID, true);
    localStorage.setItem("oo.token", "tok");
    window.location.hash = "";
    render(<App />);
    expect(screen.getByRole("heading", { name: "openOrchestra Remote" })).toBeInTheDocument();
  });
  it("reads openOrchestra Remote in the tablet top bar", () => {
    setup(ANDROID, true, true);
    localStorage.setItem("oo.token", "tok");
    render(<App />);
    expect(screen.getByRole("heading", { name: "openOrchestra Remote" })).toBeInTheDocument();
  });
});
