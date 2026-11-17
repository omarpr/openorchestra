import { afterEach, describe, expect, it } from "vitest";
import type { ConvEntry } from "./conversation";
import {
  addPending,
  arrived,
  normalize,
  type PendingMessage,
  pendingFor,
  resetPending,
  updatePending,
} from "./pending";

afterEach(() => resetPending());

const sent = (id: string, text: string, at: number): PendingMessage => ({
  id,
  agentId: "a",
  text,
  state: "sent",
  at,
  since: at,
  queued: false,
});
const user = (text: string, at: number): ConvEntry => ({
  id: `u${at}`,
  kind: "user",
  text,
  at: new Date(at).toISOString(),
});

describe("pending messages", () => {
  it("compares the way the Mac sends: newlines flattened, shell prompt ignored", () => {
    expect(normalize("fix\nthe  bug ")).toBe("fix the bug");
    expect(normalize("$ ls -la")).toBe("ls -la");
  });

  it("a message is read once the transcript has it, from its send time on", () => {
    const t = 1_800_000_000_000;
    const p = [sent("p1", "deploy it", t)];
    expect(arrived(p, [user("deploy it", t - 60_000)])).toEqual([]); // an older identical one
    expect(arrived(p, [user("deploy it", t - 60_000), user("deploy it", t + 5_000)])).toEqual(["p1"]);
  });

  it("the same text sent twice needs two copies in the transcript", () => {
    const t = 1_800_000_000_000;
    const p = [sent("p1", "yes", t), sent("p2", "yes", t + 1000)];
    expect(arrived(p, [user("yes", t + 2000)])).toEqual(["p1"]);
    expect(arrived(p, [user("yes", t + 2000), user("yes", t + 3000)])).toEqual(["p1", "p2"]);
  });

  it("only sent messages are matched (not sending or failed)", () => {
    const t = 1_800_000_000_000;
    const failed = { ...sent("p1", "x", t), state: "failed" as const };
    expect(arrived([failed], [user("x", t + 1)])).toEqual([]);
  });

  it("keeps per-agent lists stable until something changes", () => {
    const id = addPending("a", "hello", true);
    const first = pendingFor("a");
    expect(pendingFor("a")).toBe(first);
    updatePending(id, { state: "sent" });
    expect(pendingFor("a")).not.toBe(first);
    expect(pendingFor("a")[0]).toMatchObject({ text: "hello", state: "sent", queued: true });
    expect(pendingFor("b")).toEqual([]);
  });
});
