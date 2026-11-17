import { StatusBanner } from "@/components/StatusBanner";
import { type Closure, closureText, isRetryable } from "@/lib/agents";

/** Under the toolbar: why the stream ended, or that it is reconnecting; nothing while live. */
export function ConnectionState({
  closed,
  connected,
  shell,
  onRetry,
}: {
  closed: Closure | null;
  connected: boolean;
  shell: boolean;
  onRetry(): void;
}) {
  if (closed) {
    return (
      <StatusBanner tone="warning" className="border-b py-2" onRetry={isRetryable(closed) ? onRetry : undefined}>
        {closureText(closed, shell)}
      </StatusBanner>
    );
  }
  if (connected) return null;
  return (
    <StatusBanner tone="muted" className="border-b" onRetry={onRetry}>
      Reconnecting…
    </StatusBanner>
  );
}
