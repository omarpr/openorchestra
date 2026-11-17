import { EmptyState } from "@/components/EmptyState";
import { Button } from "@/components/ui/button";
import { navigate } from "@/lib/router";

/** An agent link whose tab isn't (or is no longer) in the shared list. */
export function NotShared({ showBack }: { showBack: boolean }) {
  return (
    <EmptyState
      title="This tab isn't shared"
      action={
        showBack && (
          <Button variant="outline" onClick={() => navigate("#/")}>
            All agents
          </Button>
        )
      }
    >
      It may have been closed, or remote control was turned off for it on your Mac.
    </EmptyState>
  );
}
