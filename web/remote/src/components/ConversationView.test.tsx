import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";
import type { ConvEntry } from "@/lib/conversation";

const stream: { onEvent: ((e: { event: string; data: string }) => void) | null; urls: string[]; reject: ((e: Error) => void) | null } = {
  onEvent: null,
  reject: null,
  urls: [],
};
vi.mock("@/lib/sse", () => ({
  streamEvents: (url: string, _t: string, onEvent: (e: { event: string; data: string }) => void, signal: AbortSignal) => {
    stream.urls.push(url);
    stream.onEvent = onEvent;
    return new Promise<void>((_res, rej) => {
      stream.reject = rej;
      signal.addEventListener("abort", () => rej(new Error("abort")));
    });
  },
}));
const fetchToolMock = vi.fn();
const meMock = vi.fn();
vi.mock("@/lib/api", async (orig) => ({
  ...(await orig<typeof import("@/lib/api")>()),
  fetchToolResult: (...a: unknown[]) => fetchToolMock(...a),
  me: () => meMock(),
  sendText: vi.fn(),
}));
const sockets: unknown[] = [];
vi.mock("@/lib/termSocket", () => ({
  TermSocket: class {
    fit = vi.fn();
    input = vi.fn();
    close = vi.fn();
    constructor() {
      sockets.push(this);
    }
  },
}));
vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options = {};
    element = document.createElement("div");
    loadAddon() {}
    open() {}
    onData() {
      return { dispose() {} };
    }
    reset() {}
    resize() {}
    write() {}
    dispose() {}
  },
}));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} proposeDimensions() { return undefined; } } }));

import { ConversationView } from "./ConversationView";
import { AgentView } from "./AgentView";

function push(entries: ConvEntry[], replace = false) {
  act(() => stream.onEvent?.({ event: "entries", data: JSON.stringify({ replace, entries }) }));
}

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

const agent = (over: Partial<Agent> = {}): Agent => ({
  id: "a1",
  title: "Fix login bug",
  agent_kind: "claude-code",
  project: "oo-harness",
  worktree: "wt-1",
  branch: "feat/x",
  has_conversation: true,
  status: "running",
  status_since: null,
  tool: null,
  ...over,
});

beforeEach(() => {
  stream.onEvent = null;
  stream.urls = [];
  sockets.length = 0;
  fetchToolMock.mockReset();
  meMock.mockReset();
  meMock.mockResolvedValue({ name: "p", read_only: false });
  localStorage.clear();
  localStorage.setItem("oo.token", "tok");
  mockMedia(false);
});

describe("ConversationView", () => {
  it("renders each kind", () => {
    render(<ConversationView id="a1" />);
    expect(stream.urls[0]).toBe("/api/agents/a1/conversation");
    push([
      { id: "1", kind: "user", text: "hello there" },
      { id: "2", kind: "assistant", text: "this is **b** and <script>alert(1)</script>" },
      { id: "3", kind: "tool", tool: { id: "t1", name: "Bash", summary: "cargo test", status: "ok" } },
      { id: "4", kind: "tool", tool: { id: "t2", name: "Bash", summary: "cargo build", status: "error" } },
      { id: "5", kind: "thinking", text: "line one\nline two" },
      { id: "6", kind: "thinking", duration_ms: 12400 },
      { id: "7", kind: "thinking" },
      { id: "8", kind: "notice", text: "Can't read the conversation right now" },
    ]);
    expect(screen.getByText("hello there")).toBeInTheDocument();
    const strong = screen.getByText("b");
    expect(strong.tagName).toBe("STRONG");
    expect(document.querySelector("script")).toBeNull();
    expect(screen.getByText(/<script>alert\(1\)<\/script>/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /✓ cargo test/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /✗ cargo build/ })).toBeInTheDocument();
    expect(screen.getByText(/💭 Thinking…/)).toBeInTheDocument();
    expect(screen.getByText(/line one/)).toBeInTheDocument();
    expect(screen.queryByText(/line two/)).not.toBeInTheDocument();
    expect(screen.getByText("💭 Thought for 12 s")).toBeInTheDocument();
    expect(screen.getByText("💭 Thinking (not recorded)")).toBeInTheDocument();
    expect(screen.getByText("Can't read the conversation right now")).toBeInTheDocument();
  });

  it("tap tool opens sheet with fetched output", async () => {
    fetchToolMock.mockResolvedValue({ text: "test result: ok. 5 passed", is_error: false, truncated: true });
    render(<ConversationView id="a1" />);
    push([{ id: "3", kind: "tool", tool: { id: "t1", name: "Bash", summary: "cargo test", status: "ok" } }]);
    await userEvent.click(screen.getByRole("button", { name: /cargo test/ }));
    expect(fetchToolMock).toHaveBeenCalledWith("a1", "t1");
    expect(await screen.findByText(/test result: ok\. 5 passed/)).toBeInTheDocument();
    expect(screen.getByText("…[truncated]")).toBeInTheDocument();
  });

  it("thinking expands on tap when text exists", async () => {
    render(<ConversationView id="a1" />);
    push([{ id: "5", kind: "thinking", text: "line one\nline two" }]);
    await userEvent.click(screen.getByRole("button", { name: /Thinking…/ }));
    expect(screen.getByText(/line two/)).toBeInTheDocument();
  });

  it("empty replace mid-stream shows empty list and keeps stream", () => {
    render(<ConversationView id="a1" />);
    push([{ id: "1", kind: "user", text: "hi" }]);
    push([], true);
    expect(screen.queryByText("hi")).not.toBeInTheDocument();
    expect(stream.urls).toHaveLength(1);
  });

  it("does_not_jump_when_scrolled_up", () => {
    render(<ConversationView id="a1" />);
    push([{ id: "1", kind: "user", text: "one" }]);
    const sc = screen.getByTestId("conversation-scroll");
    Object.defineProperty(sc, "scrollHeight", { value: 1000, configurable: true });
    Object.defineProperty(sc, "clientHeight", { value: 500, configurable: true });
    sc.scrollTop = 100;
    fireEvent.scroll(sc);
    push([{ id: "2", kind: "user", text: "two" }]);
    expect(sc.scrollTop).toBe(100);
    const pill = screen.getByRole("button", { name: "New messages ↓" });
    fireEvent.click(pill);
    expect(sc.scrollTop).toBe(1000);
    expect(screen.queryByRole("button", { name: "New messages ↓" })).not.toBeInTheDocument();
  });

  it("no pill for in-place upserts or empty replace while scrolled up", () => {
    render(<ConversationView id="a1" />);
    push([{ id: "t", kind: "tool", tool: { id: "t1", name: "Bash", summary: "x", status: "pending" } }]);
    const sc = screen.getByTestId("conversation-scroll");
    Object.defineProperty(sc, "scrollHeight", { value: 1000, configurable: true });
    Object.defineProperty(sc, "clientHeight", { value: 500, configurable: true });
    sc.scrollTop = 100;
    fireEvent.scroll(sc);
    push([{ id: "t", kind: "tool", tool: { id: "t1", name: "Bash", summary: "x", status: "ok" } }]);
    expect(screen.queryByRole("button", { name: "New messages ↓" })).not.toBeInTheDocument();
    push([], true);
    expect(screen.queryByRole("button", { name: "New messages ↓" })).not.toBeInTheDocument();
  });

  it("images in markdown are not rendered", () => {
    render(<ConversationView id="a1" />);
    push([{ id: "1", kind: "assistant", text: "hi ![x](http://evil.test/a.png) there" }]);
    expect(document.querySelector("img")).toBeNull();
  });

  it("sticks to bottom when at bottom", () => {
    render(<ConversationView id="a1" />);
    const sc = screen.getByTestId("conversation-scroll");
    Object.defineProperty(sc, "scrollHeight", { value: 800, configurable: true });
    Object.defineProperty(sc, "clientHeight", { value: 500, configurable: true });
    push([{ id: "1", kind: "user", text: "one" }]);
    expect(sc.scrollTop).toBe(800);
    expect(screen.queryByRole("button", { name: "New messages ↓" })).not.toBeInTheDocument();
  });

  it("revoked closes and clears token", async () => {
    const onClosed = vi.fn();
    render(<ConversationView id="a1" onClosed={onClosed} />);
    await act(async () => {
      stream.onEvent?.({ event: "closed", data: JSON.stringify({ reason: "revoked" }) });
      stream.onEvent = null;
    });
    // stream promise never resolves in this mock; closed reason is reported immediately
    expect(onClosed).toHaveBeenCalledWith("revoked");
    expect(localStorage.getItem("oo.token")).toBeNull();
  });
});

describe("AgentView conversation switch", () => {
  it("agent header shows repo · branch · agent", () => {
    render(<AgentView id="a1" agent={agent()} />);
    const h = screen.getByRole("heading", { level: 2 });
    expect(h).toHaveTextContent("oo-harness · feat/x · Claude Code");
    expect(screen.getByRole("img", { name: "claude-code" })).toBeInTheDocument();
  });

  it("header falls back to worktree and the slug", () => {
    render(<AgentView id="a1" agent={agent({ branch: null, agent_kind: "weird" })} />);
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent("oo-harness · wt-1 · weird");
  });

  it("has_conversation defaults to Conversation, terminal not connected", () => {
    render(<AgentView id="a1" agent={agent()} />);
    expect(screen.getByRole("tab", { name: "Conversation" })).toHaveAttribute("aria-selected", "true");
    expect(stream.urls).toEqual(["/api/agents/a1/conversation"]);
    expect(sockets).toHaveLength(0);
  });

  it("switching to Terminal connects the socket and persists the choice", async () => {
    render(<AgentView id="a1" agent={agent()} />);
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(sockets).toHaveLength(1);
    expect(localStorage.getItem("oo.view.a1")).toBe("terminal");
  });

  it("saved choice wins", () => {
    localStorage.setItem("oo.view.a1", "terminal");
    render(<AgentView id="a1" agent={agent()} />);
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(sockets).toHaveLength(1);
  });

  it("no socket before the agent list arrives (cold deep link)", () => {
    const { rerender } = render(<AgentView id="a1" />);
    expect(sockets).toHaveLength(0);
    expect(stream.urls).toHaveLength(0);
    rerender(<AgentView id="a1" agent={agent()} />);
    expect(sockets).toHaveLength(0);
    expect(stream.urls).toHaveLength(1);
  });

  it("closed is scoped per view; real unmarked applies to both", async () => {
    render(<AgentView id="a1" agent={agent()} />);
    act(() => stream.onEvent?.({ event: "closed", data: JSON.stringify({ reason: "unmarked" }) }));
    expect(screen.getByRole("status")).toHaveTextContent(/Remote control was turned off/);
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(screen.getByRole("status")).toHaveTextContent(/Remote control was turned off/);
  });

  it("a conversation 404 does not close the terminal view", async () => {
    render(<AgentView id="a1" agent={agent()} />);
    // simulate the hook's synthetic closure by rejecting the stream with HTTP 404
    stream.reject?.(new Error("HTTP 404"));
    await screen.findByText(/conversation isn't available/);
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(screen.queryByText(/conversation isn't available/)).not.toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("no conversation: Terminal and no switch", () => {
    render(<AgentView id="a1" agent={agent({ has_conversation: false })} />);
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(sockets).toHaveLength(1);
    expect(stream.urls).toHaveLength(0);
  });
});
