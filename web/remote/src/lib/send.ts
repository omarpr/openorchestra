import { ApiError, sendText } from "./api";
import { addPending, updatePending } from "./pending";

/** What to tell the user when a send failed. */
export function sendErrorText(err: unknown, shell: boolean): string {
  if (err instanceof ApiError && err.code === "stopped") return shell ? "Shell closed" : "Agent stopped";
  if (err instanceof ApiError && err.code === "read_only") return "This device is watch only";
  return "Couldn't reach your Mac";
}

/** Send a tracked message (`id` from `addPending`): it shows as sent, or failed with why. */
export async function deliver(id: string, agentId: string, text: string, shell: boolean): Promise<void> {
  updatePending(id, { state: "sending", error: undefined, since: Date.now() });
  try {
    const at = await sendText(agentId, text);
    updatePending(id, { state: "sent", at });
  } catch (err) {
    updatePending(id, { state: "failed", error: sendErrorText(err, shell) });
  }
}

/** Track and send `text` to `agentId`. */
export function sendTracked(agentId: string, text: string, opts: { queued: boolean; shell: boolean }) {
  const id = addPending(agentId, text, opts.queued);
  return deliver(id, agentId, text, opts.shell);
}
