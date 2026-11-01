import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { RefreshCw } from "lucide-react";
import {
  agentRequest,
  agentTerminalUrl,
  type AgentTerminal as Snapshot,
} from "../api/agents";
import {
  defaultInteractiveTerminalOptions,
  enableTerminalIme,
} from "../lib/terminal";
import { useI18n } from "../i18n";
import "@xterm/xterm/css/xterm.css";

type Props = {
  id: string;
  name: string;
  vmId: string;
  controllerId: string;
  closed: boolean;
  canControl: boolean;
};

export default function AgentTerminal({
  id,
  name,
  vmId,
  controllerId,
  closed,
  canControl,
}: Props) {
  const { t } = useI18n();
  const container = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const connect = useRef<((retry?: boolean) => void) | null>(null);
  const allowed = useRef(false);
  const live = useRef(false);
  const [state, setState] = useState(() => (closed ? "closed" : "connecting"));
  const [error, setError] = useState("");

  useEffect(() => {
    allowed.current = canControl && !closed;
    if (terminal.current) {
      terminal.current.options.disableStdin = !allowed.current || !live.current;
      if (terminal.current.textarea)
        terminal.current.textarea.disabled = !allowed.current || !live.current;
    }
    if (allowed.current) connect.current?.();
  }, [canControl, closed]);

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let disposed = false;
    let socket: WebSocket | null = null;
    let attempted = false;
    let firstFrame = true;
    let lastSequence = 0;
    let failure = false;
    let startupFrame = 0;
    const term = new Terminal(
      defaultInteractiveTerminalOptions({
        disableStdin: true,
        convertEol: false,
        cursorBlink: true,
        scrollback: 0,
        theme: {
          background: "#191b20",
          foreground: "#e4e4e7",
          cursor: "#e4e4e7",
          selectionBackground: "#52525b",
        },
      }),
    );
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(element);
    terminal.current = term;
    enableTerminalIme(term, element);
    term.textarea?.setAttribute(
      "aria-label",
      `${name} ${t("terminal input", "터미널 입력")}`,
    );
    if (term.textarea) term.textarea.disabled = true;
    const send = (value: object) => {
      if (
        !allowed.current ||
        !live.current ||
        socket?.readyState !== WebSocket.OPEN
      )
        return;
      if (socket.bufferedAmount >= 1048576) {
        failure = true;
        live.current = false;
        term.options.disableStdin = true;
        setError(
          t(
            "Terminal input is congested. Inspect the terminal before reconnecting; input was not retried.",
            "터미널 입력이 밀려 연결을 중단했다. 다시 연결하기 전에 터미널을 확인한다. 입력은 다시 전송하지 않는다.",
          ),
        );
        socket.close();
        return;
      }
      socket.send(JSON.stringify(value));
    };
    const bytes = (data: Uint8Array) => {
      if (data.length > 262144) {
        setError(
          t(
            "Paste is limited to 256 KiB. No input was sent.",
            "붙여넣기는 256 KiB까지 지원한다. 입력을 전송하지 않았다.",
          ),
        );
        return;
      }
      setError("");
      // Keep bracketed paste in one record so Herdr applies the CLI's paste mode.
      let binary = "";
      for (let index = 0; index < data.length; index += 16384) {
        binary += String.fromCharCode(...data.subarray(index, index + 16384));
      }
      send({ type: "terminal.input", bytes: btoa(binary) });
    };
    const data = term.onData((value) => bytes(new TextEncoder().encode(value)));
    const binary = term.onBinary((value) =>
      bytes(Uint8Array.from(value, (c) => c.charCodeAt(0))),
    );
    const size = () => ({
      cols: Math.max(2, Math.min(400, term.cols)),
      rows: Math.max(2, Math.min(200, term.rows)),
    });
    const resized = term.onResize(() =>
      send({ type: "terminal.resize", ...size() }),
    );
    let resizeFrame = 0;
    const resize = () => {
      cancelAnimationFrame(resizeFrame);
      resizeFrame = requestAnimationFrame(() => {
        if (!disposed && element.clientWidth && element.clientHeight) fit.fit();
      });
    };
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    resize();

    const coordinates = (event: MouseEvent) => {
      const rect = element
        .querySelector(".xterm-screen")
        ?.getBoundingClientRect();
      if (!rect || !rect.width || !rect.height) return null;
      return {
        column: Math.max(
          0,
          Math.min(
            term.cols - 1,
            Math.floor((event.clientX - rect.left) / (rect.width / term.cols)),
          ),
        ),
        row: Math.max(
          0,
          Math.min(
            term.rows - 1,
            Math.floor((event.clientY - rect.top) / (rect.height / term.rows)),
          ),
        ),
        modifiers:
          (event.shiftKey ? 1 : 0) |
          (event.ctrlKey ? 2 : 0) |
          (event.altKey ? 4 : 0),
      };
    };
    const mouse = (event: MouseEvent) => {
      // Shift keeps ordinary browser selection available inside mouse-aware CLIs.
      if (event.shiftKey || (event.type === "mousemove" && !event.buttons))
        return;
      const position = coordinates(event);
      if (position)
        send({
          type: "terminal.mouse",
          action:
            event.type === "mousedown"
              ? "down"
              : event.type === "mouseup"
                ? "up"
                : "drag",
          button:
            event.button === 2
              ? "right"
              : event.button === 1
                ? "middle"
                : "left",
          ...position,
        });
    };
    const wheel = (event: WheelEvent) => {
      if (event.ctrlKey || !allowed.current || !live.current || !event.deltaY)
        return;
      const position = coordinates(event);
      if (!position) return;
      event.preventDefault();
      event.stopPropagation();
      send({
        type: "terminal.scroll",
        direction: event.deltaY < 0 ? "up" : "down",
        lines: Math.max(
          1,
          Math.min(
            20,
            Math.ceil(Math.abs(event.deltaY) / (event.deltaMode ? 1 : 24)),
          ),
        ),
        ...position,
      });
    };
    element.addEventListener("mousedown", mouse);
    element.addEventListener("mouseup", mouse);
    element.addEventListener("mousemove", mouse);
    element.addEventListener("wheel", wheel, { passive: false, capture: true });

    const open = (retry = false) => {
      if (disposed || closed || !allowed.current || (attempted && !retry))
        return;
      socket?.close();
      attempted = true;
      live.current = false;
      term.options.disableStdin = true;
      if (term.textarea) term.textarea.disabled = true;
      firstFrame = true;
      lastSequence = 0;
      failure = false;
      setError("");
      setState("connecting");
      fit.fit();
      const { cols, rows } = size();
      const ws = new WebSocket(
        agentTerminalUrl(vmId, id, controllerId, cols, rows),
      );
      socket = ws;
      ws.onmessage = (event) => {
        if (disposed || socket !== ws) return;
        try {
          const frame = JSON.parse(String(event.data));
          if (
            frame.type === "terminal.frame" &&
            frame.encoding === "ansi" &&
            Number.isSafeInteger(frame.seq) &&
            frame.seq > lastSequence &&
            typeof frame.bytes === "string"
          ) {
            const value = Uint8Array.from(atob(frame.bytes), (c) =>
              c.charCodeAt(0),
            );
            lastSequence = frame.seq;
            if (firstFrame) {
              if (!frame.full)
                throw Error(
                  "Terminal snapshot is missing. Reconnect to the session.",
                );
              term.reset();
              // Herdr decodes paste and applies the destination CLI's paste mode.
              term.write("\x1b[?2004h");
              firstFrame = false;
            }
            term.write(value);
            live.current = true;
            term.options.disableStdin = !allowed.current;
            if (term.textarea) term.textarea.disabled = !allowed.current;
            setState("connected");
            // Resize can occur while the initial controller handshake is in flight.
            if (frame.width !== term.cols || frame.height !== term.rows)
              send({ type: "terminal.resize", ...size() });
          } else if (
            frame.type === "terminal.error" ||
            frame.type === "terminal.closed"
          ) {
            failure = true;
            setError(
              String(
                frame.message || frame.reason || "Terminal connection ended.",
              ),
            );
            ws.close();
          } else throw Error("Invalid terminal frame.");
        } catch (e) {
          failure = true;
          setError(e instanceof Error ? e.message : String(e));
          ws.close();
        }
      };
      ws.onclose = () => {
        if (disposed || socket !== ws) return;
        live.current = false;
        term.options.disableStdin = true;
        if (term.textarea) term.textarea.disabled = true;
        setState("disconnected");
        if (!failure)
          setError(
            t(
              "Terminal disconnected. Reconnect to the existing session; keystrokes are not replayed.",
              "터미널 연결이 끊겼다. 다시 연결하면 기존 세션을 연다. 입력은 다시 전송하지 않는다.",
            ),
          );
      };
      ws.onerror = () => {
        if (!disposed && socket === ws && !failure) {
          failure = true;
          setError(
            t(
              "Could not connect to this agent terminal. Check the native service and Herdr in this MicroVM.",
              "에이전트 터미널에 연결하지 못했다. 이 MicroVM의 Herdr와 Firecrab 서비스를 확인한다.",
            ),
          );
        }
      };
    };
    connect.current = open;
    if (closed) {
      void agentRequest<Snapshot>(
        `/${encodeURIComponent(id)}`,
        undefined,
        undefined,
        vmId,
      )
        .then((snapshot) => {
          if (!disposed) term.write(snapshot.text.replace(/\r?\n/g, "\r\n"));
        })
        .catch((e) => {
          if (!disposed) setError(String(e));
        });
    } else startupFrame = requestAnimationFrame(() => open());
    return () => {
      disposed = true;
      connect.current = null;
      live.current = false;
      socket?.close();
      observer.disconnect();
      cancelAnimationFrame(resizeFrame);
      cancelAnimationFrame(startupFrame);
      element.removeEventListener("mousedown", mouse);
      element.removeEventListener("mouseup", mouse);
      element.removeEventListener("mousemove", mouse);
      element.removeEventListener("wheel", wheel, true);
      data.dispose();
      binary.dispose();
      resized.dispose();
      term.dispose();
      terminal.current = null;
    };
  }, [id, name, vmId, controllerId, closed, t]);

  return (
    <div className="am-terminal-session">
      <div
        ref={container}
        className="am-terminal"
        role="log"
        aria-label={`${name} ${t("terminal output", "터미널 출력")}`}
      />
      {(error || state !== "connected" || !canControl) && (
        <div className="am-terminal-status" role={error ? "status" : undefined}>
          <span>
            {error ||
              (closed
                ? t("Session ended", "종료된 세션")
                : !canControl
                  ? t(
                      "Read only — waiting for this host's connection",
                      "읽기 전용 · 호스트 연결 대기 중",
                    )
                  : t("Connecting terminal…", "터미널 연결 중…"))}
          </span>
          {!closed && state === "disconnected" && (
            <button
              type="button"
              className="icon-btn"
              disabled={!canControl}
              aria-label={t("Reconnect terminal", "터미널 다시 연결")}
              title={t("Reconnect terminal", "터미널 다시 연결")}
              onClick={() => connect.current?.(true)}
            >
              <RefreshCw aria-hidden="true" />
            </button>
          )}
        </div>
      )}
    </div>
  );
}
