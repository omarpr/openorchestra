// Device token storage. localStorage may throw (private mode); fall back to memory.
const KEY = "oo.token";
let mem: string | null = null;
const listeners = new Set<() => void>();

export function getToken(): string | null {
  try {
    return localStorage.getItem(KEY) ?? mem;
  } catch {
    return mem;
  }
}

export function setToken(token: string): void {
  mem = token;
  try {
    localStorage.setItem(KEY, token);
  } catch {
    /* memory only */
  }
  listeners.forEach((l) => l());
}

export function clearToken(): void {
  mem = null;
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* ignore */
  }
  listeners.forEach((l) => l());
}

/** For useSyncExternalStore. */
export function subscribeToken(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}
