/**
 * A laptop window with a terminal and a chat bubble reaching a phone: "your Mac's tabs, here".
 * Drawn with theme tokens, so it follows light and dark mode.
 */
export function RemoteIllustration({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 240 160" fill="none" aria-hidden="true" className={className}>
      {/* Mac window */}
      <rect x="20" y="22" width="150" height="104" rx="10" fill="var(--card)" stroke="var(--border)" strokeWidth="2" />
      <path d="M20 42h150" stroke="var(--border)" strokeWidth="2" />
      <circle cx="34" cy="32" r="3.5" fill="var(--muted-foreground)" opacity=".35" />
      <circle cx="46" cy="32" r="3.5" fill="var(--muted-foreground)" opacity=".35" />
      <circle cx="58" cy="32" r="3.5" fill="var(--muted-foreground)" opacity=".35" />
      {/* terminal lines */}
      <path d="M36 60l8 6-8 6" stroke="var(--oo-accent)" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
      <rect x="52" y="63" width="54" height="6" rx="3" fill="var(--muted-foreground)" opacity=".45" />
      <rect x="36" y="82" width="96" height="6" rx="3" fill="var(--muted-foreground)" opacity=".25" />
      <rect x="36" y="96" width="72" height="6" rx="3" fill="var(--muted-foreground)" opacity=".25" />
      {/* signal */}
      <path d="M178 58c8 6 8 18 0 24" stroke="var(--oo-accent)" strokeWidth="3" strokeLinecap="round" opacity=".5" />
      <path
        d="M186 50c14 10 14 30 0 40"
        stroke="var(--oo-accent)"
        strokeWidth="3"
        strokeLinecap="round"
        opacity=".25"
      />
      {/* phone */}
      <rect
        x="168"
        y="70"
        width="52"
        height="84"
        rx="10"
        fill="var(--background)"
        stroke="var(--foreground)"
        strokeOpacity=".7"
        strokeWidth="2.5"
      />
      <rect x="186" y="76" width="16" height="3" rx="1.5" fill="var(--foreground)" opacity=".4" />
      <rect x="176" y="90" width="30" height="12" rx="6" fill="var(--oo-accent)" />
      <rect x="182" y="108" width="30" height="12" rx="6" fill="var(--muted)" stroke="var(--border)" />
      <rect x="176" y="126" width="22" height="12" rx="6" fill="var(--oo-accent)" opacity=".6" />
    </svg>
  );
}
