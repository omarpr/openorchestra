import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { backoffMs } from "@/hooks/useAgents";
import { applyCtrl } from "@/lib/keys";
import { TermSocket } from "@/lib/termSocket";
import { clearToken, getToken } from "@/lib/token";

export interface TerminalHandle {
  input(s: string): void;
  focus(): void;
}

interface Props {
  id: string;
  /** Fit to phone: measure the container and ask the server to resize the window. */
  fit: boolean;
  fontSize: number;
  /** Sticky Ctrl: applied to the next single typed character. */
  ctrl: boolean;
  onCtrlUsed(): void;
  onInit(readOnly: boolean): void;
  onClosed(reason: string): void;
  onConnection(connected: boolean): void;
}

const MAX_ZOOM = 4;
const BASE_FONT = 12;

function theme() {
  const dark = window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? true;
  return dark
    ? { background: "#0a0a0a", foreground: "#e5e5e5", cursor: "#e5e5e5" }
    : { background: "#ffffff", foreground: "#171717", cursor: "#171717", selectionBackground: "#bfdbfe" };
}

export const TerminalView = forwardRef<TerminalHandle, Props>(function TerminalView(props, ref) {
  const { id } = props;
  const p = useRef(props);
  p.current = props;
  const outer = useRef<HTMLDivElement>(null);
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fitAddon = useRef<FitAddon | null>(null);
  const sock = useRef<TermSocket | null>(null);
  const desktop = useRef({ cols: 80, rows: 24 });
  const readOnly = useRef(true);
  const [zoom, setZoom] = useState(1);
  const [box, setBox] = useState({ w: 0, h: 0, s: 1 });
  const zoomRef = useRef(1);
  zoomRef.current = zoom;
  const measureRef = useRef<() => void>(() => {});
  const fitNowRef = useRef<() => void>(() => {});

  useImperativeHandle(ref, () => ({
    input: (s) => {
      if (!readOnly.current) sock.current?.input(s);
    },
    focus: () => term.current?.focus(),
  }));

  // Terminal + socket lifecycle, once per agent id.
  useEffect(() => {
    const t = new Terminal({
      fontSize: p.current.fontSize,
      fontFamily: 'ui-monospace, Menlo, "SF Mono", Consolas, monospace',
      cursorBlink: false,
      scrollback: 2000,
      disableStdin: true,
      theme: theme(),
    });
    const fa = new FitAddon();
    t.loadAddon(fa);
    t.open(host.current!);
    term.current = t;
    fitAddon.current = fa;

    let disposed = false;
    let finished = false;
    let pendingFit = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let fitTimer: ReturnType<typeof setTimeout> | undefined;

    // Fit to the container. Skips (retried on the next ResizeObserver tick) when it is not laid out yet.
    const sendFit = () => {
      if (!sock.current || readOnly.current) return;
      const dims = fa.proposeDimensions();
      if (!dims || !Number.isFinite(dims.cols) || !Number.isFinite(dims.rows) || dims.cols < 2 || dims.rows < 1) return;
      if (dims.cols !== t.cols || dims.rows !== t.rows) t.resize(dims.cols, dims.rows);
      sock.current.fit(true, dims.cols, dims.rows);
    };

    const measure = () => {
      const o = outer.current;
      const screen = host.current?.querySelector<HTMLElement>(".xterm-screen");
      if (!o || !screen) return;
      if (p.current.fit) {
        setBox((b) => (b.s === 1 && b.w === 0 ? b : { w: 0, h: 0, s: 1 }));
        return;
      }
      const w = screen.offsetWidth;
      const h = screen.offsetHeight;
      if (!w || !o.clientWidth) return;
      // Scale from font size too: width grows with the font, so without the factor A-/A+ would cancel out.
      setBox({ w, h, s: (o.clientWidth / w) * (p.current.fontSize / BASE_FONT) * zoomRef.current });
    };
    measureRef.current = measure;
    fitNowRef.current = () => {
      if (p.current.fit) sendFit();
      else {
        sock.current?.fit(false, 0, 0);
        t.resize(desktop.current.cols, desktop.current.rows);
      }
      requestAnimationFrame(measure);
    };

    const dataSub = t.onData((d) => {
      if (readOnly.current) return;
      let out = d;
      if (p.current.ctrl && d.length === 1) {
        out = applyCtrl(d);
        p.current.onCtrlUsed();
      }
      sock.current?.input(out);
    });

    const connect = () => {
      if (disposed || finished) return;
      const token = getToken();
      if (!token) return;
      const proto = location.protocol === "https:" ? "wss:" : "ws:";
      const s = new TermSocket(`${proto}//${location.host}/api/term/${encodeURIComponent(id)}`, token, {
        onInit(cols, rows, ro) {
          if (disposed || sock.current !== s) return;
          t.reset();
          readOnly.current = ro;
          t.options.disableStdin = ro;
          desktop.current = { cols, rows };
          attempt = 0;
          // Keep the init size so the repaint (drawn at that size) lands intact; fit goes after it is written.
          t.resize(cols, rows);
          pendingFit = p.current.fit && !ro;
          p.current.onConnection(true);
          p.current.onInit(ro);
          requestAnimationFrame(measure);
        },
        onData(u8) {
          if (disposed || sock.current !== s) return;
          if (pendingFit) {
            pendingFit = false;
            t.write(u8, () => {
              if (!disposed && sock.current === s && p.current.fit) sendFit();
            });
          } else t.write(u8);
        },
        onClosed(reason) {
          if (disposed || sock.current !== s) return;
          finished = true;
          if (reason === "revoked") clearToken();
          p.current.onConnection(false);
          p.current.onClosed(reason);
        },
        onDisconnect(code) {
          if (disposed || finished || sock.current !== s) return;
          if (code === 4401) {
            finished = true;
            clearToken();
            return;
          }
          p.current.onConnection(false);
          timer = setTimeout(connect, backoffMs(attempt++));
        },
      });
      sock.current = s;
    };

    const onVisible = () => {
      if (document.visibilityState !== "visible" || disposed || finished) return;
      attempt = 0;
      clearTimeout(timer);
      sock.current?.close();
      sock.current = null;
      connect();
    };
    document.addEventListener("visibilitychange", onVisible);

    const ro = new ResizeObserver(() => {
      clearTimeout(fitTimer);
      fitTimer = setTimeout(() => {
        if (disposed) return;
        if (p.current.fit) sendFit();
        measure();
      }, 150);
    });
    if (outer.current) ro.observe(outer.current);

    connect();
    return () => {
      disposed = true;
      clearTimeout(timer);
      clearTimeout(fitTimer);
      document.removeEventListener("visibilitychange", onVisible);
      ro.disconnect();
      dataSub.dispose();
      sock.current?.close();
      sock.current = null;
      t.dispose();
      term.current = null;
    };
  }, [id]);

  // Fit toggle.
  useEffect(() => {
    if (!sock.current) return;
    fitNowRef.current();
  }, [props.fit]);

  // Font size.
  useEffect(() => {
    const t = term.current;
    if (!t) return;
    t.options.fontSize = props.fontSize;
    requestAnimationFrame(() => (props.fit ? fitNowRef.current() : measureRef.current()));
  }, [props.fontSize, props.fit]);

  useEffect(() => measureRef.current(), [zoom]);

  // Pinch to zoom (fit off).
  const pinch = useRef<{ d: number; z: number } | null>(null);
  const dist = (e: React.TouchEvent) =>
    Math.hypot(e.touches[0].clientX - e.touches[1].clientX, e.touches[0].clientY - e.touches[1].clientY);

  const scaled = !props.fit && box.w > 0;
  return (
    <div
      ref={outer}
      className="h-full w-full overflow-auto"
      style={{ touchAction: "pan-x pan-y", background: theme().background }}
      onTouchStart={(e) => {
        pinch.current = e.touches.length === 2 ? { d: dist(e), z: zoomRef.current } : null;
      }}
      onTouchMove={(e) => {
        if (e.touches.length === 2 && pinch.current && pinch.current.d > 0) {
          setZoom(Math.min(MAX_ZOOM, Math.max(1, (pinch.current.z * dist(e)) / pinch.current.d)));
        }
      }}
      onTouchEnd={() => (pinch.current = null)}
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
