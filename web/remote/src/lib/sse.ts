import { ApiError, errorCode, UnauthorizedError } from "./api";

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
  const events = frames.map(parseFrame).filter((e): e is SseEvent => e !== null);
  return { events, carry: rest };
}

/** One frame's `event:` and `data:` lines; `null` without data (comments, keep-alives). */
function parseFrame(frame: string): SseEvent | null {
  let event = "message";
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line === "" || line.startsWith(":")) continue;
    const i = line.indexOf(":");
    const field = i < 0 ? line : line.slice(0, i);
    const value = i < 0 ? "" : line.slice(i + 1).replace(/^ /, "");
    if (field === "event") event = value;
    else if (field === "data") data.push(value);
  }
  return data.length > 0 ? { event, data: data.join("\n") } : null;
}

/** JSON in an event's data, or `null` for a malformed frame. */
export function parseData<T>(data: string): T | null {
  try {
    return JSON.parse(data) as T;
  } catch {
    return null;
  }
}

/** The reason in a `closed` event (`{"reason":"…"}`, or the raw text from an older server). */
export function closedReason(data: string): string {
  const j = parseData<{ reason?: string }>(data);
  return j ? (j.reason ?? "") : data;
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
  // 403 says why (e.g. `untrusted_network`): callers show it.
  if (res.status === 403) throw new ApiError(403, await errorCode(res));
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let carry = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    const r = parseSse(dec.decode(value, { stream: true }), carry);
    carry = r.carry;
    for (const e of r.events) onEvent(e);
  }
}
