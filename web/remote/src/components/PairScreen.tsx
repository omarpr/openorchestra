import { useRef, useState } from "react";
import { ApiError, pair } from "@/lib/api";
import { defaultDeviceName } from "@/lib/device";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { InputOTP, InputOTPGroup, InputOTPSeparator, InputOTPSlot } from "@/components/ui/input-otp";

export const MSG_BAD_CODE = "That code didn't work — check Settings → Remote on your Mac";
export const MSG_LOCKED = "Too many tries — make a new code on your Mac";

function messageFor(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 423) return MSG_LOCKED;
    if (e.status === 401) return MSG_BAD_CODE;
    if (e.status === 429) return "Too many tries — wait a minute and try again";
  }
  return "Couldn't reach your Mac. Check that you're on the same network.";
}

/** On success the token is stored and the App swaps to the agents list. */
export function PairScreen() {
  const [code, setCode] = useState("");
  const [name, setName] = useState(() => defaultDeviceName());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);

  async function submit(value: string) {
    if (inFlight.current || value.length !== 8) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      await pair(value.toUpperCase(), name.trim() || "Phone");
    } catch (e) {
      setError(messageFor(e));
      setCode("");
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }

  return (
    <main className="flex min-h-full items-center justify-center p-4 pt-[max(1rem,env(safe-area-inset-top))] pb-[max(1rem,env(safe-area-inset-bottom))]">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-xl">Pair this device</CardTitle>
          <CardDescription>Enter the 8-character code shown in Settings → Remote on your Mac.</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              void submit(code);
            }}
          >
            <InputOTP
              maxLength={8}
              value={code}
              onChange={(v) => setCode(v.toUpperCase())}
              onComplete={(v) => void submit(v)}
              pattern="^[A-Za-z0-9]*$"
              pushPasswordManagerStrategy="none"
              inputMode="text"
              autoCapitalize="characters"
              autoComplete="off"
              autoFocus
              disabled={busy}
              aria-label="Pairing code"
            >
              <InputOTPGroup>
                {[0, 1, 2, 3].map((i) => (
                  <InputOTPSlot key={i} index={i} />
                ))}
              </InputOTPGroup>
              <InputOTPSeparator />
              <InputOTPGroup>
                {[4, 5, 6, 7].map((i) => (
                  <InputOTPSlot key={i} index={i} />
                ))}
              </InputOTPGroup>
            </InputOTP>
            <div className="flex flex-col gap-1.5">
              <label htmlFor="device-name" className="text-sm font-medium">
                Device name
              </label>
              <Input id="device-name" value={name} onChange={(e) => setName(e.target.value)} maxLength={64} />
            </div>
            {error && (
              <p role="alert" className="text-sm text-destructive">
                {error}
              </p>
            )}
            <Button type="submit" disabled={busy || code.length !== 8}>
              {busy ? "Pairing…" : "Pair"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
