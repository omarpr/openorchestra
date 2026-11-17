import { describe, expect, it } from "vitest";
import { agentName, closureText, isRemoteWide, isRetryable } from "./agents";

describe("agents", () => {
  it("names agent kinds, and nothing for a shell", () => {
    expect(agentName("claude-code")).toBe("Claude Code");
    expect(agentName("weird")).toBe("weird");
    expect(agentName(null)).toBe("");
  });

  it("explains closures, shell-aware", () => {
    expect(closureText("stopped", false)).toBe("Agent stopped");
    expect(closureText("stopped", true)).toBe("Shell closed");
    expect(closureText("something-new", false)).toBe("Disconnected");
  });

  it("knows which closures a retry can clear and which end both views", () => {
    expect(["disabled", "unmarked", "x"].every(isRetryable)).toBe(true);
    expect(["revoked", "stopped", "no_conversation"].some(isRetryable)).toBe(false);
    expect(isRemoteWide("disabled")).toBe(true);
    expect(isRemoteWide("stopped")).toBe(false);
  });
});
