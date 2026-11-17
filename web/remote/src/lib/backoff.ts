/** Reconnect delay: 1 s, 2 s, 4 s … capped at 15 s. */
export const backoffMs = (attempt: number) => Math.min(1000 * 2 ** attempt, 15000);
