import { Component, type ReactNode } from "react";
import { Button } from "@/components/ui/button";

/** A failing view must leave the agent header and view switch usable. */
export class ViewErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 px-4 text-center">
        <p role="alert" className="text-sm text-muted-foreground">
          This view couldn’t open.
        </p>
        <Button variant="outline" onClick={() => window.location.reload()}>
          Reload app
        </Button>
      </div>
    );
  }
}
