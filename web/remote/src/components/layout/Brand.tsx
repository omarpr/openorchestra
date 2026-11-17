import { hostName } from "@/lib/host";

/** Logo, app name, and the Mac this app talks to. */
export function Brand() {
  const host = hostName();
  return (
    <>
      <img src="/logo-64.png" alt="" width={24} height={24} className="size-6 shrink-0 rounded-md" />
      <div className="flex min-w-0 flex-col leading-tight">
        <h1 className="font-semibold">openOrchestra Remote</h1>
        {host && <p className="truncate text-xs text-muted-foreground">{host}</p>}
      </div>
    </>
  );
}
