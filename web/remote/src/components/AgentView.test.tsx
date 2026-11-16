import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";

const calls: string[] = [];
vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options: Record<string, unknown> = {};
    element = document.createElement("div");
    loadAddon() {}
    open() {}
    onData() {
      return { dispose() {} };
    }
    reset() {
      calls.push("reset");
    }
    resize() {
      calls.push("resize");
    }
    write() {
      calls.push("write");
    }
    dispose() {}
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
    proposeDimensions() {
      return { cols: 40, rows: 20 };
    }
  },
}));

const sockets: { url: string; h: any; fit: any; input: any; close: any }[] = [];
vi.mock("@/lib/termSocket", () => ({
  TermSocket: class {
    fit = vi.fn();
    input = vi.fn();
    close = vi.fn();
    constructor(
      public url: string,
      public token: string,
      public h: any,
    ) {
      sockets.push(this as any);
    }
  },
}));

const meMock = vi.fn();
const sendTextMock = vi.fn();
vi.mock("@/lib/api", async (orig) => ({
  ...(await orig<typeof import("@/lib/api")>()),
  me: () => meMock(),
  sendText: (...a: unknown[]) => sendTextMock(...a),
}));

import { AgentView } from "./AgentView";

const agent: Agent = {
  id: "a1",
  title: "Fix login bug",
  agent_kind: "claude",
  project: "p",
  worktree: "w",
  status: "running",
  status_since: null,
  tool: null,
};

function mockMedia(matches: boolean) {
  window.matchMedia = ((q: string) => ({
    matches,
    media: q,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    onchange: null,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe("AgentView", () => {
  beforeEach(() => {
    calls.length = 0;
    sockets.length = 0;
    meMock.mockReset();
    sendTextMock.mockReset();
    localStorage.clear();
    localStorage.setItem("oo.token", "tok");
    mockMedia(false);
  });

  it("reconnects_and_resets_terminal", async () => {
    meMock.mockResolvedValue({ name: "p", read_only: false });
    render(<AgentView id="a1" agent={agent} />);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].url).toMatch(/^wss?:\/\/.*\/api\/term\/a1$/);
    act(() => sockets[0].h.onInit(100, 30, false));
    act(() => sockets[0].h.onData(new Uint8Array([65])));
    act(() => sockets[0].h.onDisconnect());
    calls.length = 0;

    act(() => {
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(sockets).toHaveLength(2);
    act(() => sockets[1].h.onInit(100, 30, false));
    act(() => sockets[1].h.onData(new Uint8Array([66])));
    expect(calls.indexOf("reset")).toBeGreaterThanOrEqual(0);
    expect(calls.indexOf("reset")).toBeLessThan(calls.indexOf("write"));
  });

  it("read-only hides key bar and quick reply, shows Watch only", async () => {
    meMock.mockResolvedValue({ name: "p", read_only: true });
    render(<AgentView id="a1" agent={agent} />);
    expect(await screen.findByText("Watch only")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Esc" })).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/message/i)).not.toBeInTheDocument();
  });

  it("writable shows key bar and quick reply sends text", async () => {
    meMock.mockResolvedValue({ name: "p", read_only: false });
    sendTextMock.mockResolvedValue(undefined);
    render(<AgentView id="a1" agent={agent} />);
    expect(await screen.findByRole("button", { name: "Esc" })).toBeInTheDocument();
    const box = screen.getByPlaceholderText(/message/i);
    await userEvent.type(box, "hello");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(sendTextMock).toHaveBeenCalledWith("a1", "hello"));
    await waitFor(() => expect(box).toHaveValue(""));
    act(() => sockets[0].h.onInit(80, 24, false));
    await userEvent.click(screen.getByRole("button", { name: "^C" }));
    expect(sockets[0].input).toHaveBeenCalledWith("\x03");
  });

  it("closed unmarked shows the banner", async () => {
    meMock.mockResolvedValue({ name: "p", read_only: false });
    render(<AgentView id="a1" agent={agent} />);
    act(() => sockets[0].h.onClosed("unmarked"));
    expect(screen.getByText("Remote control was turned off for this agent")).toBeInTheDocument();
  });
});
