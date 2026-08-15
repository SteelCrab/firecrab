import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { defaultInteractiveTerminalOptions } from "../lib/terminal";

type Status = "connecting" | "connected" | "reconnecting" | "disconnected";

const STATUS_LABEL: Record<Status, string> = {
  connecting: "연결 중…",
  connected: "실시간",
  reconnecting: "재연결 중…",
  disconnected: "연결 끊김",
};

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;

/**
 * Read-only live view of one VM's serial console, sized to sit inside a
 * panel rather than fill a page.
 *
 * This is `Console.tsx`'s connection logic with the page chrome removed —
 * see that component for the full-featured version. Two differences beyond
 * layout: input is never forwarded (the bootstrap script owns this console;
 * a stray keystroke would corrupt the heredoc it is being fed), and there
 * is no VM metadata polling, because the caller already knows which VM this
 * is and unmounts us the moment it stops existing.
 */
export default function InlineConsole({ vmId }: { vmId: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const intentionalCloseRef = useRef(false);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [status, setStatus] = useState<Status>("connecting");

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  /**
   * Fit only when the surface has a real box. Calling FitAddon.fit() with a
   * 0×0 container (first paint / StrictMode remount) sets cols/rows to 0 and
   * the terminal draws nothing until a later successful fit.
   */
  const doFit = useCallback(() => {
    const fit = fitRef.current;
    const term = termRef.current;
    const el = containerRef.current;
    if (!fit || !term || !el) return false;
    if (el.clientWidth < 16 || el.clientHeight < 16) return false;
    try {
      const proposed = fit.proposeDimensions();
      if (!proposed || proposed.cols < 2 || proposed.rows < 2) return false;
      fit.fit();
      return term.cols >= 2 && term.rows >= 2;
    } catch {
      return false;
    }
  }, []);

  const scheduleFit = useCallback(() => {
    let tries = 0;
    const tick = () => {
      if (doFit()) return;
      tries += 1;
      if (tries < 20) requestAnimationFrame(tick);
    };
    requestAnimationFrame(() => requestAnimationFrame(tick));
  }, [doFit]);

  // Terminal and socket share one effect so a StrictMode remount can never
  // leave a live socket writing into a disposed Terminal (blank screen).
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    intentionalCloseRef.current = false;
    reconnectAttemptRef.current = 0;

    const term = new Terminal(
      defaultInteractiveTerminalOptions({
        fontSize: 12,
        theme: {
          background: "#171b22",
          foreground: "#e8ecf1",
          cursor: "#c43e12",
          selectionBackground: "rgba(196, 62, 18, 0.35)",
        },
        disableStdin: true,
        cursorBlink: false,
        cols: 80,
        rows: 16,
      }),
    );
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    termRef.current = term;
    fitRef.current = fitAddon;
    scheduleFit();

    const observer = new ResizeObserver(() => {
      if (!disposed) scheduleFit();
    });
    observer.observe(container);
    const onWinResize = () => {
      if (!disposed) scheduleFit();
    };
    window.addEventListener("resize", onWinResize);

    const scheduleReconnect = () => {
      if (disposed || intentionalCloseRef.current) return;
      clearReconnectTimer();
      const attempt = reconnectAttemptRef.current;
      const delay = Math.min(RECONNECT_BASE_MS * 2 ** attempt, RECONNECT_MAX_MS);
      reconnectAttemptRef.current = attempt + 1;
      setStatus("reconnecting");
      reconnectTimerRef.current = setTimeout(() => connect(true), delay);
    };

    const connect = (isRetry: boolean) => {
      if (disposed || intentionalCloseRef.current) return;

      const previous = socketRef.current;
      if (previous) {
        socketRef.current = null;
        previous.onopen = null;
        previous.onmessage = null;
        previous.onclose = null;
        try {
          previous.close();
        } catch {
          /* ignore */
        }
      }

      setStatus(isRetry ? "reconnecting" : "connecting");
      const socket = new WebSocket(consoleWsUrl(vmId));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;

      socket.onopen = () => {
        if (disposed || socketRef.current !== socket) return;
        reconnectAttemptRef.current = 0;
        setStatus("connected");
        scheduleFit();
      };

      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        if (disposed || socketRef.current !== socket) return;
        const live = termRef.current;
        if (!live) return;
        live.write(
          typeof event.data === "string" ? event.data : new Uint8Array(event.data),
        );
      };

      socket.onclose = () => {
        if (socketRef.current === socket) socketRef.current = null;
        if (disposed || intentionalCloseRef.current) return;
        setStatus("disconnected");
        scheduleReconnect();
      };
    };

    const bootTimer = window.setTimeout(() => {
      if (!disposed) connect(false);
    }, 0);

    return () => {
      disposed = true;
      intentionalCloseRef.current = true;
      window.clearTimeout(bootTimer);
      clearReconnectTimer();
      observer.disconnect();
      window.removeEventListener("resize", onWinResize);
      const socket = socketRef.current;
      if (socket) {
        socketRef.current = null;
        socket.onopen = null;
        socket.onmessage = null;
        socket.onclose = null;
        try {
          socket.close();
        } catch {
          /* ignore */
        }
      }
      term.dispose();
      if (termRef.current === term) termRef.current = null;
      if (fitRef.current === fitAddon) fitRef.current = null;
    };
  }, [vmId, clearReconnectTimer, scheduleFit]);

  return (
    <div className="inline-console">
      <div className="inline-console-bar">
        <span className="inline-console-title">빌더 VM 콘솔</span>
        <span className={`inline-console-status ${status}`} role="status" aria-live="polite">
          {STATUS_LABEL[status]}
        </span>
      </div>
      <div className="inline-console-surface" ref={containerRef} />
    </div>
  );
}

/**
 * Same derivation as `Console.tsx` — `/ws`, not `/api`, because REST and
 * WebSocket routes can't share a proxied path prefix (see the `/ws`
 * sub-router comment in `firecrab-api/src/server.rs`).
 */
function consoleWsUrl(vmId: string): string {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/ws/vms/${vmId}/console`;
}
