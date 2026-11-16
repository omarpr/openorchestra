import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PairScreen } from "./PairScreen";

const fetchMock = vi.fn();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
  localStorage.clear();
});
afterEach(() => vi.unstubAllGlobals());

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
    expect(
      await screen.findByText("That code didn't work — check Settings → Remote on your Mac"),
    ).toBeInTheDocument();
  });

  it("shows the 423 copy", async () => {
    fetchMock.mockResolvedValue(new Response(JSON.stringify({ error: "locked" }), { status: 423 }));
    await enterCode();
    expect(
      await screen.findByText("Too many tries — make a new code on your Mac"),
    ).toBeInTheDocument();
  });
});
