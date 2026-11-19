import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

// Exercise React.lazy itself. The other tests load these views synchronously.
vi.unmock("@/components/lazyViews");
vi.mock("@/components/ConversationView", () => ({
  get ConversationView() {
    throw new Error("Conversation chunk unavailable");
  },
}));
vi.mock("@/components/TerminalView", async () => {
  const { forwardRef } = await import("react");
  return { TerminalView: forwardRef(() => <div>Terminal loaded</div>) };
});
vi.mock("@/hooks/useAgents", () => ({
  useAgents: () => ({
    agents: [
      {
        id: "codex-1",
        title: "Codex fixture",
        agent_kind: "codex",
        has_conversation: true,
        project: "Project",
        worktree: "main",
        branch: null,
        status: "idle",
        status_since: null,
        tool: null,
      },
    ],
    connected: true,
    loaded: true,
  }),
}));
vi.mock("@/lib/api", async (original) => ({
  ...(await original<typeof import("@/lib/api")>()),
  me: async () => ({ name: "Phone", read_only: true }),
}));

import App from "./App";

it("keeps navigation usable when the Codex Conversation chunk fails to load", async () => {
  window.location.hash = "#/";
  localStorage.clear();
  localStorage.setItem("oo.token", "fixture-token");
  window.matchMedia = (media): MediaQueryList => ({
    media,
    matches: false,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => true,
  });
  const ignoreFixtureError = (event: ErrorEvent) => {
    if (event.error?.message === "Conversation chunk unavailable") event.preventDefault();
  };
  window.addEventListener("error", ignoreFixtureError);
  const expectedErrors = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByRole("link", { name: /Codex fixture/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("This view couldn’t open.");
    expect(screen.getByRole("button", { name: "Reload app" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back to agents" })).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(await screen.findByText("Terminal loaded")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  } finally {
    window.removeEventListener("error", ignoreFixtureError);
    expectedErrors.mockRestore();
  }
});
