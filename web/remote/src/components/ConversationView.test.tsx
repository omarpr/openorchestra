import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Agent } from "@/lib/api";
import type { ConvEntry } from "@/lib/conversation";

const stream: {
  onEvent: ((e: { event: string; data: string }) => void) | null;
  urls: string[];
  reject: ((e: Error) => void) | null;
} = {
  onEvent: null,
  reject: null,
  urls: [],
};
vi.mock("@/lib/sse", async (orig) => ({
  ...(await orig<typeof import("@/lib/sse")>()),
  streamEvents: (
    url: string,
    _t: string,
    onEvent: (e: { event: string; data: string }) => void,
    signal: AbortSignal,
  ) => {
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
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
    proposeDimensions() {
      return undefined;
    }
  },
}));

import { AgentView } from "./AgentView";
import { ConversationView } from "./ConversationView";

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
    expect(screen.getByRole("button", { name: "✓ Bash cargo test" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "✗ Bash cargo build" })).toBeInTheDocument();
    expect(screen.getByText(/💭 Thinking…/)).toBeInTheDocument();
    expect(screen.getByText(/line one/)).toBeInTheDocument();
    expect(screen.queryByText(/line two/)).not.toBeInTheDocument();
    expect(screen.getByText("💭 Thought for 12 s")).toBeInTheDocument();
    expect(screen.getByText("💭 Thinking (not recorded)")).toBeInTheDocument();
    expect(screen.getByText("Can't read the conversation right now")).toBeInTheDocument();
  });

  it("tool lines show a per-tool glyph before the summary, the name otherwise", () => {
    render(<ConversationView id="a1" />);
    const t = (id: string, name: string, summary: string) =>
      ({ id, kind: "tool", tool: { id, name, summary, status: "ok" } }) as ConvEntry;
    push([
      t("1", "Edit", "src/a.rs"),
      t("2", "Read", "src/b.rs"),
      t("3", "Bash", "cargo test"),
      t("4", "Grep", '"needle"'),
      t("5", "WebFetch", "docs.rs"),
      t("6", "Task", "explore"),
      t("7", "TodoWrite", "TodoWrite"),
      t("8", "bash", "ls"),
      t("9", "mcp__x__go", "now"),
      t("10", "Mystery", ""),
      t("11", "mcp__y__z", "mcp__y__z"),
    ]);
    const row = (name: string) => screen.getByRole("button", { name }).textContent;
    expect(row("✓ Edit src/a.rs")).toBe("✎src/a.rs");
    expect(row("✓ Read src/b.rs")).toBe("📄src/b.rs");
    expect(row("✓ Bash cargo test")).toBe("$cargo test");
    expect(row('✓ Grep "needle"')).toBe('🔍"needle"');
    expect(row("✓ WebFetch docs.rs")).toBe("🌐docs.rs");
    expect(row("✓ Task explore")).toBe("🤖explore");
    expect(row("✓ TodoWrite")).toBe("☑TodoWrite");
    expect(row("✓ bash ls")).toBe("$ls");
    expect(row("✓ mcp__x__go now")).toBe("mcp__x__gonow");
    // Blank summary, or one that is just the name: the name once, never a blank line.
    expect(row("✓ Mystery")).toBe("Mystery");
    expect(row("✓ mcp__y__z")).toBe("mcp__y__z");
  });

  it("thought for under half a second reads 1 s, not 0 s", () => {
    render(<ConversationView id="a1" />);
    push([
      { id: "1", kind: "thinking", duration_ms: 300 },
      { id: "2", kind: "thinking", duration_ms: 0 },
    ]);
    expect(screen.getAllByText("💭 Thought for 1 s")).toHaveLength(2);
  });

  it("a pending tool's sheet says Still running and fetches once it finishes", async () => {
    fetchToolMock.mockResolvedValue({ text: "done", is_error: false, truncated: false });
    render(<ConversationView id="a1" />);
    push([{ id: "3", kind: "tool", tool: { id: "t1", name: "Bash", summary: "sleep 9", status: "pending" } }]);
    await userEvent.click(screen.getByRole("button", { name: /sleep 9/ }));
    expect(await screen.findByText("Still running")).toBeInTheDocument();
    expect(fetchToolMock).not.toHaveBeenCalled();
    expect(screen.queryByText("Output isn't available")).not.toBeInTheDocument();
    push([{ id: "3", kind: "tool", tool: { id: "t1", name: "Bash", summary: "sleep 9", status: "ok" } }]);
    expect(await screen.findByText("done")).toBeInTheDocument();
    expect(fetchToolMock).toHaveBeenCalledWith("a1", "t1");
  });

  it("stays at the bottom when the list area resizes while stuck", () => {
    const observers: { cb: ResizeObserverCallback; els: Element[] }[] = [];
    const Orig = globalThis.ResizeObserver;
    const g = globalThis as { ResizeObserver: unknown };
    g.ResizeObserver = class {
      els: Element[] = [];
      constructor(cb: ResizeObserverCallback) {
        observers.push({ cb, els: this.els });
      }
      observe(el: Element) {
        this.els.push(el);
      }
      unobserve() {}
      disconnect() {}
    };
    try {
      render(<ConversationView id="a1" />);
      push([{ id: "1", kind: "user", text: "one" }]);
      const sc = screen.getByTestId("conversation-scroll");
      Object.defineProperty(sc, "scrollHeight", { value: 1000, configurable: true });
      Object.defineProperty(sc, "clientHeight", { value: 500, configurable: true });
      sc.scrollTop = 500;
      fireEvent.scroll(sc);
      const fire = () =>
        act(() => {
          for (const o of observers.filter((o) => o.els.includes(sc))) o.cb([], {} as ResizeObserver);
        });
      // The keyboard opens: the scroller shrinks and the browser leaves scrollTop alone.
      Object.defineProperty(sc, "clientHeight", { value: 200, configurable: true });
      fire();
      expect(sc.scrollTop).toBe(1000);
      // Scrolled up: a resize leaves the reader where they are.
      sc.scrollTop = 100;
      fireEvent.scroll(sc);
      fire();
      expect(sc.scrollTop).toBe(100);
    } finally {
      g.ResizeObserver = Orig;
    }
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

  it("returning to a view clears its own closure but not a global one", async () => {
    render(<AgentView id="a1" agent={agent()} />);
    stream.reject?.(new Error("HTTP 404"));
    await screen.findByText(/conversation isn't available/);
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    await userEvent.click(screen.getByRole("tab", { name: "Conversation" }));
    expect(screen.queryByText(/conversation isn't available/)).not.toBeInTheDocument();
    expect(stream.urls).toHaveLength(2);
    act(() => stream.onEvent?.({ event: "closed", data: JSON.stringify({ reason: "unmarked" }) }));
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    await userEvent.click(screen.getByRole("tab", { name: "Conversation" }));
    expect(screen.getByRole("status")).toHaveTextContent(/Remote control was turned off/);
  });

  it("opens the conversation when the initial transcript arrives", () => {
    const { rerender } = render(<AgentView id="a1" agent={agent({ has_conversation: false })} />);
    expect(sockets).toHaveLength(1);
    expect(localStorage.getItem("oo.view.a1")).toBeNull();
    rerender(<AgentView id="a1" agent={agent({ has_conversation: true })} />);
    expect(screen.getByRole("tab", { name: "Conversation" })).toHaveAttribute("aria-selected", "true");
    expect(sockets).toHaveLength(1);
    expect(stream.urls).toHaveLength(1);
  });

  it("nothing is saved while the agent list is still loading", () => {
    render(<AgentView id="a1" />);
    expect(localStorage.getItem("oo.view.a1")).toBeNull();
  });

  it.each(["claude-code", "codex"])("offers Conversation for %s while its transcript is pending", async (kind) => {
    const { rerender } = render(<AgentView id="a1" agent={agent({ agent_kind: kind, has_conversation: false })} />);
    await userEvent.click(screen.getByRole("tab", { name: "Conversation" }));
    expect(screen.getByText(/Waiting for a transcript/)).toBeInTheDocument();
    expect(stream.urls).toHaveLength(0);
    rerender(<AgentView id="a1" agent={agent({ agent_kind: kind })} />);
    expect(stream.urls).toEqual(["/api/agents/a1/conversation"]);
  });

  it("unsupported agent: Terminal and no switch", () => {
    render(<AgentView id="a1" agent={agent({ agent_kind: "kimi", has_conversation: false })} />);
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(sockets).toHaveLength(1);
    expect(stream.urls).toHaveLength(0);
  });

  it("waiting focuses the reply input once per transition", async () => {
    const { rerender } = render(<AgentView id="a1" agent={agent({ status: "running" })} />);
    const input = () => screen.getByLabelText("Message Claude Code");
    await screen.findByLabelText("Message Claude Code");
    expect(input()).not.toHaveFocus();
    rerender(<AgentView id="a1" agent={agent({ status: "waiting" })} />);
    expect(input()).toHaveFocus();
    input().blur();
    rerender(<AgentView id="a1" agent={agent({ status: "waiting", tool: "x" })} />);
    expect(input()).not.toHaveFocus();
    rerender(<AgentView id="a1" agent={agent({ status: "running" })} />);
    rerender(<AgentView id="a1" agent={agent({ status: "waiting" })} />);
    expect(input()).toHaveFocus();
  });

  it("shows the status bar between the list and the reply box", () => {
    render(<AgentView id="a1" agent={agent({ status: "running", tool: "Bash: ls" })} />);
    expect(screen.getByText("Working…")).toBeInTheDocument();
    expect(screen.getByText("Bash: ls")).toBeInTheDocument();
  });

  it("while the agent list is loading, the reply box is hidden", () => {
    render(<AgentView id="a1" />);
    expect(screen.queryByLabelText("Message Claude Code")).not.toBeInTheDocument();
    expect(screen.queryByText("This tab isn't shared")).not.toBeInTheDocument();
  });

  it("once the list loaded and the id is missing: not-shared state with a way back", async () => {
    render(<AgentView id="a1" agentsLoaded />);
    expect(screen.getByText("This tab isn't shared")).toBeInTheDocument();
    expect(screen.queryByLabelText("Message Claude Code")).not.toBeInTheDocument();
    expect(stream.urls).toHaveLength(0);
    expect(sockets).toHaveLength(0);
    await userEvent.click(screen.getByRole("button", { name: "All agents" }));
    expect(window.location.hash).toBe("#/");
  });
});
