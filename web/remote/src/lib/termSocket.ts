export interface TermHandlers {
  onInit(cols: number, rows: number, readOnly: boolean): void;
  onData(data: Uint8Array): void;
  onClosed(reason: string): void;
  /** Socket ended without a `closed` frame. `code` is the WebSocket close code when known (4401 = bad auth). */
  onDisconnect(code?: number): void;
}

/** Thin client for `/api/term/:id`. Silent after `close()` and after a `closed` frame. */
export class TermSocket {
  private ws: WebSocket;
  private done = false;

  constructor(url: string, token: string, private h: TermHandlers) {
    const ws = new WebSocket(url);
    this.ws = ws;
    ws.binaryType = "arraybuffer";
    ws.onopen = () => ws.send(JSON.stringify({ type: "auth", token }));
    ws.onmessage = (e: MessageEvent) => {
      if (this.done) return;
      const d = e.data;
      if (typeof d === "string") {
        let m: { type?: string; cols?: number; rows?: number; read_only?: boolean; reason?: string };
        try {
          m = JSON.parse(d);
        } catch {
          return;
        }
        if (m.type === "init") this.h.onInit(m.cols ?? 80, m.rows ?? 24, !!m.read_only);
        else if (m.type === "closed") {
          this.done = true;
          this.h.onClosed(m.reason ?? "");
        }
      } else if (d instanceof ArrayBuffer) {
        this.h.onData(new Uint8Array(d));
      }
    };
    ws.onclose = (e: CloseEvent) => {
      if (this.done) return;
      this.done = true;
      this.h.onDisconnect(e.code);
    };
    ws.onerror = () => {};
  }

  private send(o: object) {
    if (!this.done && this.ws.readyState === 1) this.ws.send(JSON.stringify(o));
  }

  input(s: string) {
    this.send({ type: "input", data: s });
  }

  fit(on: boolean, cols: number, rows: number) {
    this.send(on ? { type: "fit", on: true, cols, rows } : { type: "fit", on: false });
  }

  close() {
    this.done = true;
    try {
      this.ws.close();
    } catch {
      /* already closed */
    }
  }
}
