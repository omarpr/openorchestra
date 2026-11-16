export function ConnectionBanner({ connected, disabled }: { connected: boolean; disabled: boolean }) {
  if (connected && !disabled) return null;
  const text = disabled ? "Remote control is off on the Mac" : "Reconnecting…";
  return (
    <div role="status" className="bg-amber-100 px-4 py-2 text-center text-sm text-amber-950 dark:bg-amber-950 dark:text-amber-100">
      {text}
    </div>
  );
}
