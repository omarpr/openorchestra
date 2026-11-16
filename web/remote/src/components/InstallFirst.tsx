import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

/** iOS only: pairing inside Safari doesn't carry over to the Home Screen app, so install first. */
export function InstallFirst({ onPairHere }: { onPairHere: () => void }) {
  return (
    <main className="flex min-h-full items-center justify-center p-4 pt-[max(1rem,env(safe-area-inset-top))] pb-[max(1rem,env(safe-area-inset-bottom))]">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-xl">Add openOrchestra to your Home Screen</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <ol className="list-decimal space-y-2 pl-5 text-sm">
            <li>Tap Share</li>
            <li>Choose Add to Home Screen</li>
            <li>Open openOrchestra from your Home Screen and pair there</li>
          </ol>
          <button
            type="button"
            onClick={onPairHere}
            className="-mx-2 flex min-h-11 items-center self-start px-2 text-xs text-muted-foreground underline underline-offset-2"
          >
            Pair in this browser instead
          </button>
        </CardContent>
      </Card>
    </main>
  );
}
