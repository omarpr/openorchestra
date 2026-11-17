import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { useCallback, useEffect, useRef, useState } from "react";
import { isDark } from "@/lib/appearance";
import { backoffMs } from "@/lib/backoff";
import { applyCtrl } from "@/lib/keys";
import { terminalTheme as theme } from "@/lib/terminalTheme";
import { TermSocket } from "@/lib/termSocket";
import { clearToken, getToken } from "@/lib/token";

export interface TerminalSessionProps {
  /** Fit to screen: measure the container and ask the server to resize the window. */
  fit: boolean;
  fontSize: number;
  /** Sticky Ctrl: applied to the next single typed character. */
  ctrl: boolean;
  onCtrlUsed(): void;
  onInit(readOnly: boolean): void;
  onClosed(reason: string): void;
  onConnection(connected: boolean): void;
}

/** Unscaled font size the "fit off" scaling is measured against. */
export const BASE_FONT = 12;

/**
 * The xterm instance and its socket for agent `id`: created once per id, reconnecting with
 * backoff, re-fitting on resize, and handing the desktop its size back when the page hides.
 * Attach `outer` (the scroll box) and `host` (where xterm draws); `box` is the scale for
 * "fit off" (the desktop-sized terminal shrunk to the container). `zoom` is the pinch zoom and
 * `dark` the appearance, applied live.
 */
export function useTerminalSession(id: string, props: TerminalSessionProps, zoom: number, dark: boolean) {
  const p = useRef(props);
  p.current = props;
  const outer = useRef<HTMLDivElement>(null);
  const host = useRef<HTMLDivElement>(null);
  const zoomRef = useRef(zoom);
  const term = useRef<Terminal | null>(null);
  const fitAddon = useRef<FitAddon | null>(null);
  const sock = useRef<TermSocket | null>(null);
  const desktop = useRef({ cols: 80, rows: 24 });
  const readOnly = useRef(true);
  const [box, setBox] = useState({ w: 0, h: 0, s: 1 });
  const measureRef = useRef<() => void>(() => {});
  const fitNowRef = useRef<(fit: boolean) => void>(() => {});

  // Terminal + socket lifecycle, once per agent id.
  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const t = new Terminal({
      fontSize: p.current.fontSize,
      fontFamily: 'ui-monospace, Menlo, "SF Mono", Consolas, monospace',
      cursorBlink: false,
      scrollback: 2000,
      disableStdin: true,
      theme: theme(isDark()),
    });
    const fa = new FitAddon();
    t.loadAddon(fa);
    t.open(el);
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
      // Never re-fit a hidden page (a pending resize timer, a reconnect's init, or the fit/font effects): the
      // desktop got its size back on hide, and the socket may stay up answering pings.
      if (!sock.current || readOnly.current || document.visibilityState === "hidden") return;
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
    fitNowRef.current = (fit) => {
      if (fit) sendFit();
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

    // Backgrounded or locked: give the desktop its window size back now (the server would only notice a silent
    // phone after its idle limit). The switch stays on; coming back reconnects, and the new init re-fits.
    const releaseFit = () => {
      if (disposed || !p.current.fit || readOnly.current) return;
      sock.current?.fit(false, 0, 0);
    };
    const onVisible = () => {
      if (document.visibilityState === "hidden") {
        releaseFit();
        return;
      }
      if (document.visibilityState !== "visible" || disposed || finished) return;
      attempt = 0;
      clearTimeout(timer);
      sock.current?.close();
      sock.current = null;
      connect();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("pagehide", releaseFit);

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
      window.removeEventListener("pagehide", releaseFit);
      ro.disconnect();
      dataSub.dispose();
      sock.current?.close();
      sock.current = null;
      t.dispose();
      term.current = null;
    };
  }, [id]);

  // Appearance.
  useEffect(() => {
    if (term.current) term.current.options.theme = theme(dark);
  }, [dark]);

  // Fit toggle.
  useEffect(() => {
    if (sock.current) fitNowRef.current(props.fit);
  }, [props.fit]);

  // Font size: refit or rescale once the new size is drawn.
  useEffect(() => {
    const t = term.current;
    if (!t) return;
    t.options.fontSize = props.fontSize;
    requestAnimationFrame(() => (props.fit ? fitNowRef.current(true) : measureRef.current()));
  }, [props.fontSize, props.fit]);

  // Pinch zoom (fit off).
  useEffect(() => {
    zoomRef.current = zoom;
    measureRef.current();
  }, [zoom]);

  /** Typed input from outside xterm (the key bar), unless watch-only. */
  const input = useCallback((s: string) => {
    if (!readOnly.current) sock.current?.input(s);
  }, []);
  const focus = useCallback(() => term.current?.focus(), []);

  return { outer, host, box, input, focus };
}
