import { StatusBanner } from "@/components/StatusBanner";

/** The agent list's connection: hidden while live. */
export function ConnectionBanner({
  connected,
  disabled,
  untrusted = false,
  onRetry,
}: {
  connected: boolean;
  disabled: boolean;
  /** The Mac refuses the network this device is on. */
  untrusted?: boolean;
  onRetry?: () => void;
}) {
  if (connected && !disabled) return null;
  return (
    <StatusBanner tone="warning" onRetry={onRetry} className="justify-center text-center [&>span]:flex-none">
      {untrusted
        ? "Your Mac doesn't trust this network. On the Mac: Settings → Remote → Networks → Trust."
        : disabled
          ? "Remote control is off on the Mac"
          : "Reconnecting…"}
    </StatusBanner>
  );
}
