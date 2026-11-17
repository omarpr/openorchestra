// Device token storage. localStorage may throw (private mode); fall back to memory.
import { readPref, writePref } from "./storage";

const KEY = "oo.token";
let mem: string | null = null;
const listeners = new Set<() => void>();

export function getToken(): string | null {
  return readPref(KEY) ?? mem;
}

export function setToken(token: string): void {
  mem = token;
  writePref(KEY, token);
  for (const l of listeners) l();
}

export function clearToken(): void {
  mem = null;
  writePref(KEY, null);
  for (const l of listeners) l();
}

/** For useSyncExternalStore. */
export function subscribeToken(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}
