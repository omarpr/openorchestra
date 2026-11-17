// Messages sent from this device that the agent's transcript doesn't show yet. Claude Code
// records a typed message only when it reads it (later, if it was busy), so without these the
// conversation would look as if nothing was sent.
import type { ConvEntry } from "./conversation";
import { readPref, writePref } from "./storage";

export type PendingState = "sending" | "sent" | "failed";

export interface PendingMessage {
  id: string;
  agentId: string;
  text: string;
  state: PendingState;
  /** When the Mac typed it (its clock, ms); until then, when it was sent. */
  at: number;
  /** When this device sent it (its own clock, ms): how long it has waited. */
  since: number;
  /** The agent was working when it was sent: it waits its turn. */
  queued: boolean;
  /** Why it failed, for the user. */
  error?: string;
}

const KEY = "oo.pending";
/** Forgotten after this long either way (sessions end; agents get closed). */
const KEEP_MS = 30 * 60 * 1000;
/** Clock slack between the Mac's `Date` header (whole seconds) and transcript times. */
const SLACK_MS = 2000;

let all: PendingMessage[] = load();
const listeners = new Set<() => void>();
const byAgent = new Map<string, PendingMessage[]>();

function load(): PendingMessage[] {
  try {
    const list = JSON.parse(readPref(KEY) ?? "[]") as PendingMessage[];
    const now = Date.now();
    // A send interrupted by a reload: it may or may not have arrived.
    return list
      .filter((m) => now - (m.since ?? m.at) < KEEP_MS)
      .map((m) =>
        m.state === "sending"
          ? { ...m, state: "failed", error: "Interrupted. Check the conversation before sending again." }
          : m,
      );
  } catch {
    return [];
  }
}

function commit(next: PendingMessage[]) {
  all = next;
  byAgent.clear();
  writePref(KEY, all.length ? JSON.stringify(all) : null);
  for (const l of listeners) l();
}

export function subscribePending(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/** This agent's pending messages, oldest first (a stable array between changes). */
export function pendingFor(agentId: string): PendingMessage[] {
  let list = byAgent.get(agentId);
  if (!list) {
    list = all.filter((m) => m.agentId === agentId);
    byAgent.set(agentId, list);
  }
  return list;
}

export function addPending(agentId: string, text: string, queued: boolean): string {
  const id = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const now = Date.now();
  commit([...all, { id, agentId, text, state: "sending", at: now, since: now, queued }]);
  return id;
}

export function updatePending(id: string, patch: Partial<Omit<PendingMessage, "id" | "agentId">>) {
  commit(all.map((m) => (m.id === id ? { ...m, ...patch } : m)));
}

export function removePending(ids: Iterable<string>) {
  const gone = new Set(ids);
  if (gone.size) commit(all.filter((m) => !gone.has(m.id)));
}

/** Compare as sent: newlines became spaces; a shell's entry reads "$ cmd". */
export function normalize(text: string): string {
  return text.replace(/^\$ /, "").replace(/\s+/g, " ").trim();
}

/**
 * Pending messages the transcript now shows: each sent one is matched, oldest first, to a
 * user entry with the same text from its send time on, each entry at most once (so sending
 * the same thing twice needs two entries).
 */
export function arrived(pending: PendingMessage[], entries: ConvEntry[]): string[] {
  const users = entries
    .filter((e) => e.kind === "user" && e.text)
    .map((e) => ({ text: normalize(e.text ?? ""), at: e.at ? Date.parse(e.at) : Number.NaN }));
  const claimed = new Set<number>();
  const out: string[] = [];
  for (const m of [...pending].sort((a, b) => a.at - b.at)) {
    if (m.state !== "sent") continue;
    const want = normalize(m.text);
    const i = users.findIndex(
      (u, j) => !claimed.has(j) && u.text === want && (Number.isNaN(u.at) || u.at >= m.at - SLACK_MS),
    );
    if (i >= 0) {
      claimed.add(i);
      out.push(m.id);
    }
  }
  return out;
}

/** For tests. */
export function resetPending() {
  commit([]);
}
