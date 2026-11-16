import { describe, expect, it } from "vitest";
import { parseSse } from "./sse";

describe("parseSse", () => {
  it("handles a frame split across two chunks", () => {
    const a = parseSse("event: agents\ndata: [1,", "");
    expect(a.events).toEqual([]);
    const b = parseSse("2]\n\n", a.carry);
    expect(b.events).toEqual([{ event: "agents", data: "[1,2]" }]);
    expect(b.carry).toBe("");
  });

  it("parses multiple frames in one chunk and keeps the tail as carry", () => {
    const r = parseSse("event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\nda", "");
    expect(r.events).toEqual([
      { event: "a", data: "1" },
      { event: "b", data: "2" },
    ]);
    expect(r.carry).toBe("event: c\nda");
  });

  it("ignores :keepalive comments", () => {
    const r = parseSse(":keepalive\n\nevent: agents\ndata: []\n\n", "");
    expect(r.events).toEqual([{ event: "agents", data: "[]" }]);
  });

  it("joins multi-line data with newlines", () => {
    const r = parseSse("event: x\ndata: one\ndata: two\n\n", "");
    expect(r.events).toEqual([{ event: "x", data: "one\ntwo" }]);
  });

  it("handles CRLF and defaults the event name to message", () => {
    const r = parseSse("data: hi\r\n\r\n", "");
    expect(r.events).toEqual([{ event: "message", data: "hi" }]);
  });
});
