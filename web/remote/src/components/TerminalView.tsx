import { forwardRef, useImperativeHandle, useSyncExternalStore } from "react";
import "@xterm/xterm/css/xterm.css";
import { usePinchZoom } from "@/hooks/usePinchZoom";
import { type TerminalSessionProps, useTerminalSession } from "@/hooks/useTerminalSession";
import { isDark, subscribeAppearance } from "@/lib/appearance";
import { terminalTheme } from "@/lib/terminalTheme";

export interface TerminalHandle {
  input(s: string): void;
  focus(): void;
}

interface Props extends TerminalSessionProps {
  id: string;
}

const MAX_ZOOM = 4;

/** The live terminal: fitted to the screen, or the desktop's size scaled down (pinch to zoom). */
export const TerminalView = forwardRef<TerminalHandle, Props>(function TerminalView({ id, ...props }, ref) {
  const { zoom, handlers: pinchHandlers } = usePinchZoom(MAX_ZOOM);
  const dark = useSyncExternalStore(subscribeAppearance, isDark);
  const { outer, host, box, input, focus } = useTerminalSession(id, props, zoom, dark);
  useImperativeHandle(ref, () => ({ input, focus }), [input, focus]);

  const scaled = !props.fit && box.w > 0;
  return (
    <div
      ref={outer}
      className="h-full w-full overflow-auto"
      style={{ touchAction: "pan-x pan-y", background: terminalTheme(dark).background }}
      {...pinchHandlers}
    >
      <div style={scaled ? { width: box.w * box.s, height: box.h * box.s } : { width: "100%", height: "100%" }}>
        <div
          ref={host}
          data-testid="terminal"
          style={
            scaled
              ? { width: box.w, height: box.h, transform: `scale(${box.s})`, transformOrigin: "0 0" }
              : { width: "100%", height: "100%" }
          }
        />
      </div>
    </div>
  );
});
