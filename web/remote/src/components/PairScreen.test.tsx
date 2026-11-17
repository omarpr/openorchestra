import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PairScreen } from "./PairScreen";

const fetchMock = vi.fn();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
  localStorage.clear();
});
afterEach(async () => {
  vi.unstubAllGlobals();
  // input-otp schedules setTimeout()s (selection mirroring) that can fire after
  // vitest tears the jsdom window down ("window is not defined", exit 1).
  // Unmount, then let them drain while the environment is still alive.
  cleanup();
  await new Promise((r) => setTimeout(r, 150));
});

async function enterCode() {
  const user = userEvent.setup();
  render(<PairScreen />);
  await user.clear(screen.getByLabelText("Device name"));
  await user.type(screen.getByLabelText("Device name"), "My Phone");
  await user.click(screen.getByLabelText("Pairing code"));
  await user.keyboard("abcd2345");
}

describe("PairScreen", () => {
  it("submits the upper-cased code with the device name", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ token: "T" }), { status: 200 }));
    await enterCode();
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("/api/pair");
    expect(JSON.parse(init.body)).toEqual({ code: "ABCD2345", device_name: "My Phone" });
    await waitFor(() => expect(localStorage.getItem("oo.token")).toBe("T"));
  });

  it("shows the 401 copy", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ error: "invalid" }), { status: 401 }));
    await enterCode();
    expect(await screen.findByText("That code didn't work — check Settings → Remote on your Mac")).toBeInTheDocument();
  });

  it("shows the 423 copy", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ error: "locked" }), { status: 423 }));
    await enterCode();
    expect(await screen.findByText("Too many tries — make a new code on your Mac")).toBeInTheDocument();
  });

  it("says when only this device is locked out", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ error: "address_locked" }), { status: 423 }));
    await enterCode();
    expect(
      await screen.findByText("Too many wrong codes from this device — try again in 10 minutes"),
    ).toBeInTheDocument();
  });
});

describe("Mac name", () => {
  afterEach(() => document.querySelector('meta[name="oo-host"]')?.remove());

  it("names the Mac being paired with when the server provides it", () => {
    const m = document.createElement("meta");
    m.name = "oo-host";
    m.content = "Studio";
    document.head.append(m);
    render(<PairScreen />);
    expect(screen.getByText("Pair with Studio")).toBeInTheDocument();
  });

  it("falls back to a generic title without it", () => {
    render(<PairScreen />);
    expect(screen.getByText("Pair this device")).toBeInTheDocument();
  });
});
