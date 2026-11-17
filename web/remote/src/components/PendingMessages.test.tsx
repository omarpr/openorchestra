import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ConvEntry } from "@/lib/conversation";
import { addPending, pendingFor, resetPending, updatePending } from "@/lib/pending";
import { PendingMessages, STUCK_MS } from "./PendingMessages";

const sendText = vi.hoisted(() => vi.fn());
vi.mock("@/lib/api", async (orig) => ({ ...(await orig<typeof import("@/lib/api")>()), sendText }));

beforeEach(() => sendText.mockReset());
afterEach(() => {
  resetPending();
  vi.useRealTimers();
});

describe("PendingMessages", () => {
  it("shows sending, then sent and queued", () => {
    const id = addPending("a1", "run the tests", true);
    render(<PendingMessages agentId="a1" entries={[]} shell={false} />);
    expect(screen.getByText("run the tests")).toBeInTheDocument();
    expect(screen.getByText("Sending…")).toBeInTheDocument();
    act(() => updatePending(id, { state: "sent" }));
    expect(screen.getByText(/queued, Claude reads it after the current step/)).toBeInTheDocument();
  });

  it("disappears once the transcript shows the message", () => {
    const id = addPending("a1", "ship it", false);
    act(() => updatePending(id, { state: "sent", at: 1000 }));
    const { rerender } = render(<PendingMessages agentId="a1" entries={[]} shell={false} />);
    expect(screen.getByText(/Sent · waiting for Claude/)).toBeInTheDocument();
    const read: ConvEntry = { id: "u1", kind: "user", text: "ship it", at: new Date(2000).toISOString() };
    rerender(<PendingMessages agentId="a1" entries={[read]} shell={false} />);
    expect(screen.queryByText("ship it")).not.toBeInTheDocument();
    expect(pendingFor("a1")).toEqual([]);
  });

  it("a failed message can be retried or discarded", async () => {
    const id = addPending("a1", "hello", false);
    act(() => updatePending(id, { state: "failed", error: "Couldn't reach your Mac" }));
    render(<PendingMessages agentId="a1" entries={[]} shell={false} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Not sent: Couldn't reach your Mac");
    sendText.mockResolvedValue(5000);
    await userEvent.click(screen.getByRole("button", { name: /Retry/ }));
    expect(sendText).toHaveBeenCalledWith("a1", "hello");
    expect(await screen.findByText(/Sent · waiting for Claude/)).toBeInTheDocument();

    act(() => updatePending(id, { state: "failed", error: "x" }));
    await userEvent.click(screen.getByRole("button", { name: /Discard/ }));
    expect(screen.queryByText("hello")).not.toBeInTheDocument();
  });

  it("suggests the terminal when a message isn't picked up", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const id = addPending("a1", "continue", false);
    act(() => updatePending(id, { state: "sent" }));
    const open = vi.fn();
    render(<PendingMessages agentId="a1" entries={[]} shell={false} onOpenTerminal={open} />);
    act(() => {
      vi.advanceTimersByTime(STUCK_MS + 15_000);
    });
    expect(screen.getByText(/Not picked up yet/)).toBeInTheDocument();
    screen.getByRole("button", { name: "Open terminal" }).click();
    expect(open).toHaveBeenCalled();
  });

  it("speaks of the shell in a shell tab", () => {
    const id = addPending("s1", "ls", true);
    act(() => updatePending(id, { state: "sent" }));
    render(<PendingMessages agentId="s1" entries={[]} shell />);
    expect(screen.getByText(/the shell reads it/)).toBeInTheDocument();
  });
});
