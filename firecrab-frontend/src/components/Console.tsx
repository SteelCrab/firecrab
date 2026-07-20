import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

type Status = "connecting" | "connected" | "disconnected" | "failed";

const STATUS_LABEL: Record<Status, string> = {
  connecting: "연결 중…",
  connected: "연결됨",
  disconnected: "연결 끊김",
  failed: "연결 실패",
};

const STATUS_CLASS: Record<Status, string> = {
  connecting: "connecting",
  connected: "connected",
  disconnected: "error",
  failed: "error",
};

interface ConsoleProps {
  vmId: string;
  vmName: string;
  onClose: () => void;
}

/**
 * Serial console panel: opens a WebSocket to the VM's console endpoint,
 * streams the guest's ttyS0 into an xterm.js terminal, and forwards typed
 * input back over the same socket.
 */
export default function Console({ vmId, vmName, onClose }: ConsoleProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<Status>("connecting");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    setStatus("connecting");

    const term = new Terminal({
      convertEol: true,
      fontFamily: '"IBM Plex Mono", ui-monospace, monospace',
      fontSize: 13,
      theme: { background: "#171b22", foreground: "#e8ecf1" },
      scrollback: 5000,
    });
    term.open(container);

    const socket = new WebSocket(consoleWsUrl(vmId));
    // Default is 'blob'; without this, event.data below is a Blob, not the
    // ArrayBuffer we need to hand xterm.js.
    socket.binaryType = "arraybuffer";

    socket.onopen = () => setStatus("connected");
    socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
      term.write(typeof event.data === "string" ? event.data : new Uint8Array(event.data));
    };
    socket.onclose = () => setStatus("disconnected");
    socket.onerror = () => setStatus("failed");

    const dataListener = term.onData((data) => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(new TextEncoder().encode(data));
      }
    });

    return () => {
      dataListener.dispose();
      socket.close();
      term.dispose();
    };
  }, [vmId]);

  return (
    <div className="console-overlay">
      <div className="console-panel">
        <div className="console-bar">
          <span className="console-title">{`terminal — ${vmName}`}</span>
          <span className={`console-status ${STATUS_CLASS[status]}`}>{STATUS_LABEL[status]}</span>
          <button className="btn console-close" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="console-surface" ref={containerRef}></div>
      </div>
    </div>
  );
}

/**
 * `wss://…` (or `ws://…` over plain HTTP) at the same host/port the page was
 * served from, so the dev proxy and same-origin production hosting both
 * work without a hardcoded API origin. Lives under `/ws`, not `/api` — see
 * the comment on the `/ws` sub-router in `firecrab-api/src/server.rs` for
 * why REST and WebSocket routes can't share a proxied path prefix.
 */
function consoleWsUrl(vmId: string): string {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/ws/vms/${vmId}/console`;
}
