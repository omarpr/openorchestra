import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TermSocket } from "./termSocket";

class MockWS {
  static last: MockWS;
  sent: string[] = [];
  readyState = 0;
  binaryType = "blob";
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: ((e: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    MockWS.last = this;
  }
  send(s: string) {
    this.sent.push(s);
  }
  close() {
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  msg(data: unknown) {
    this.onmessage?.({ data });
  }
}

function handlers() {
  return { onInit: vi.fn(), onData: vi.fn(), onClosed: vi.fn(), onDisconnect: vi.fn() };
}

describe("TermSocket", () => {
  beforeEach(() => vi.stubGlobal("WebSocket", MockWS));
  afterEach(() => vi.unstubAllGlobals());

  it("sends the auth frame first", () => {
    const s = new TermSocket("wss://h/api/term/a", "t", handlers());
    MockWS.last.open();
    expect(MockWS.last.sent[0]).toBe('{"type":"auth","token":"t"}');
    expect(MockWS.last.binaryType).toBe("arraybuffer");
    s.close();
  });

  it("dispatches init, binary data in order, and closed", () => {
    const h = handlers();
    new TermSocket("wss://h/x", "t", h);
    const ws = MockWS.last;
    ws.open();
    ws.msg('{"type":"init","cols":100,"rows":30,"read_only":true}');
    ws.msg(new Uint8Array([1]).buffer);
    ws.msg(new Uint8Array([2]).buffer);
    expect(h.onInit).toHaveBeenCalledWith(100, 30, true);
    expect([...h.onData.mock.calls[0][0]]).toEqual([1]);
    expect([...h.onData.mock.calls[1][0]]).toEqual([2]);
    ws.msg('{"type":"closed","reason":"unmarked"}');
    expect(h.onClosed).toHaveBeenCalledWith("unmarked");
    ws.onclose?.({ code: 1000 });
    expect(h.onDisconnect).not.toHaveBeenCalled();
  });

  it("reports disconnect with the close code and frames input/fit", () => {
    const h = handlers();
    const s = new TermSocket("wss://h/x", "t", h);
    const ws = MockWS.last;
    ws.open();
    s.input("ls\r");
    s.fit(true, 50, 20);
    s.fit(false, 0, 0);
    expect(JSON.parse(ws.sent[1])).toEqual({ type: "input", data: "ls\r" });
    expect(JSON.parse(ws.sent[2])).toEqual({ type: "fit", on: true, cols: 50, rows: 20 });
    expect(JSON.parse(ws.sent[3])).toEqual({ type: "fit", on: false });
    ws.onclose?.({ code: 4401 });
    expect(h.onDisconnect).toHaveBeenCalledWith(4401);
  });

  it("is silent after close()", () => {
    const h = handlers();
    const s = new TermSocket("wss://h/x", "t", h);
    const ws = MockWS.last;
    s.close();
    ws.onclose?.({ code: 1006 });
    expect(h.onDisconnect).not.toHaveBeenCalled();
  });
});
