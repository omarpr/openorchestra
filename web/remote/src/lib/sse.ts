import { UnauthorizedError } from "./api";

export interface SseEvent {
  event: string;
  data: string;
}

/** Incremental SSE parser. `carry` is the unfinished tail to pass with the next chunk. */
export function parseSse(chunk: string, carry: string): { events: SseEvent[]; carry: string } {
  let raw = carry + chunk;
  // A lone trailing \r may be the first half of a CRLF split across chunks.
  const heldCr = raw.endsWith("\r");
  if (heldCr) raw = raw.slice(0, -1);
  const frames = raw.replace(/\r\n?/g, "\n").split("\n\n");
  const rest = (frames.pop() ?? "") + (heldCr ? "\r" : "");
  const events: SseEvent[] = [];
  for (const frame of frames) {
    let event = "message";
    const data: string[] = [];
    for (const line of frame.split("\n")) {
      if (line === "" || line.startsWith(":")) continue;
      const i = line.indexOf(":");
      const field = i < 0 ? line : line.slice(0, i);
      let value = i < 0 ? "" : line.slice(i + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") {
        event = value;
      } else if (field === "data") {
        data.push(value);
      }
    }
    if (data.length > 0) events.push({ event, data: data.join("\n") });
  }
  return { events, carry: rest };
}

/**
 * Stream SSE over fetch (EventSource can't set Authorization). Resolves when the
 * stream ends; rejects with UnauthorizedError on 401 or Error on other failures.
 */
export async function streamEvents(
  url: string,
  token: string,
  onEvent: (e: SseEvent) => void,
  signal: AbortSignal,
): Promise<void> {
  const res = await fetch(url, {
    headers: { Authorization: `Bearer ${token}`, Accept: "text/event-stream" },
    signal,
  });
  if (res.status === 401) throw new UnauthorizedError();
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let carry = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    const r = parseSse(dec.decode(value, { stream: true }), carry);
    carry = r.carry;
    r.events.forEach(onEvent);
  }
}
