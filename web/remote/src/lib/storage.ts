// Per-device preferences in localStorage. Storage can throw (private mode, quota); every caller
// treats a failure as "not saved", so these never throw.

export function readPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** `null` removes the key. */
export function writePref(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* per-device convenience only */
  }
}
