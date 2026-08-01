import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { EgressPolicy, VmResponse } from "../bindings";
import { getVm } from "../api/client";

type Status = "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";

const STATUS_LABEL: Record<Status, string> = {
  connecting: "연결 중…",
  connected: "연결됨",
  reconnecting: "재연결 중…",
  disconnected: "연결 끊김",
  failed: "연결 실패",
};

const STATUS_CLASS: Record<Status, string> = {
  connecting: "connecting",
  connected: "connected",
  reconnecting: "connecting",
  disconnected: "error",
  failed: "error",
};

const EGRESS_LABEL: Record<EgressPolicy, string> = {
  internet: "인터넷 허용",
  isolated: "격리",
};

/** Named color schemes — font family stays IBM Plex Mono to match the shell. */
const THEMES: Record<string, { label: string; theme: ITheme }> = {
  ember: {
    label: "Ember",
    theme: {
      background: "#171b22",
      foreground: "#e8ecf1",
      cursor: "#c43e12",
      selectionBackground: "rgba(196, 62, 18, 0.35)",
    },
  },
  classic: {
    label: "Classic",
    theme: {
      background: "#0c0c0c",
      foreground: "#cccccc",
      cursor: "#ffffff",
      selectionBackground: "rgba(255, 255, 255, 0.25)",
    },
  },
  light: {
    label: "Light",
    theme: {
      background: "#f3f5f7",
      foreground: "#171b22",
      cursor: "#c43e12",
      selectionBackground: "rgba(196, 62, 18, 0.2)",
    },
  },
  solarized: {
    label: "Solarized",
    theme: {
      background: "#002b36",
      foreground: "#839496",
      cursor: "#93a1a1",
      selectionBackground: "rgba(147, 161, 161, 0.3)",
    },
  },
};

type ThemeId = keyof typeof THEMES;

const FONT_SIZES = [12, 13, 14, 16, 18] as const;
const PREFS_KEY = "firecrab.console.prefs";
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;
const VM_POLL_MS = 3_000;

interface ConsolePrefs {
  fontSize: number;
  themeId: ThemeId;
}

const DEFAULT_PREFS: ConsolePrefs = { fontSize: 13, themeId: "ember" };

function loadPrefs(): ConsolePrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return DEFAULT_PREFS;
    const parsed = JSON.parse(raw) as Partial<ConsolePrefs>;
    const fontSize = FONT_SIZES.includes(parsed.fontSize as (typeof FONT_SIZES)[number])
      ? (parsed.fontSize as number)
      : DEFAULT_PREFS.fontSize;
    const themeId =
      parsed.themeId && parsed.themeId in THEMES ? parsed.themeId : DEFAULT_PREFS.themeId;
    return { fontSize, themeId };
  } catch {
    return DEFAULT_PREFS;
  }
}

function savePrefs(prefs: ConsolePrefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* private mode / quota — ignore */
  }
}

interface ConsoleProps {
  vmId: string;
  /** Leave the console page (usually back to `#/vms`). */
  onClose: () => void;
}

/**
 * Serial console as a dedicated page (`#/console/<vmId>`), normally opened
 * in a **new browser window** from the VM list.
 *
 * Layout: toolbar + xterm surface + bottom VM detail panel. "터미널만"
 * hides chrome (bar + details) so only the terminal remains.
 */
export default function Console({ vmId, onClose }: ConsoleProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const pageRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const intentionalCloseRef = useRef(false);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const initialPrefsRef = useRef(loadPrefs());

  const [status, setStatus] = useState<Status>("connecting");
  const [prefs, setPrefs] = useState<ConsolePrefs>(initialPrefsRef.current);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [reconnectKey, setReconnectKey] = useState(0);
  const [vm, setVm] = useState<VmResponse | null>(null);
  const [vmError, setVmError] = useState<string | null>(null);
  /** Hide toolbar + detail panel — terminal fills the window. */
  const [terminalOnly, setTerminalOnly] = useState(false);
  const terminalOnlyRef = useRef(false);
  useEffect(() => {
    terminalOnlyRef.current = terminalOnly;
  }, [terminalOnly]);

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  /**
   * Fit after layout paints. Single rAF is not enough when the browser
   * or the flex surface changes size — the container still reports the old
   * box for one frame, and FitAddon keeps a too-large canvas that then clips
   * under `overflow: hidden`.
   */
  const scheduleFit = useCallback(() => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        try {
          fitRef.current?.fit();
        } catch {
          /* unmounted mid-fit */
        }
      });
    });
  }, []);

  // Poll VM metadata for the bottom detail panel + document title.
  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const next = await getVm(vmId);
        if (cancelled) return;
        setVm(next);
        setVmError(null);
        document.title = `terminal — ${next.name}`;
      } catch (error) {
        if (cancelled) return;
        setVmError(error instanceof Error ? error.message : String(error));
      }
    };
    void refresh();
    const interval = setInterval(() => void refresh(), VM_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [vmId]);

  // Terminal lifecycle: create once per mount; prefs applied live below.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const initial = initialPrefsRef.current;
    const term = new Terminal({
      convertEol: true,
      fontFamily: '"IBM Plex Mono", ui-monospace, monospace',
      fontSize: initial.fontSize,
      theme: THEMES[initial.themeId].theme,
      scrollback: 5000,
      cursorBlink: true,
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    termRef.current = term;
    fitRef.current = fitAddon;
    scheduleFit();

    const dataListener = term.onData((data) => {
      // Drop xterm's synthetic CPR answer so it is not echoed by the guest
      // tty into an `ESC[row;colR` garbage loop at the prompt.
      // eslint-disable-next-line no-control-regex -- matching a literal ESC byte is the point
      if (/^\x1b\[\d+;\d+R$/.test(data)) return;
      const socket = socketRef.current;
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(new TextEncoder().encode(data));
      }
    });

    const ro = new ResizeObserver(() => scheduleFit());
    ro.observe(container);
    const page = pageRef.current;
    if (page) ro.observe(page);

    const onWinResize = () => scheduleFit();
    window.addEventListener("resize", onWinResize);

    // Escape leaves terminal-only mode so chrome is recoverable without a mouse.
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.ctrlKey || event.metaKey || event.altKey) return;
      if (!terminalOnlyRef.current) return;
      event.preventDefault();
      setTerminalOnly(false);
    };
    window.addEventListener("keydown", onKey);

    return () => {
      dataListener.dispose();
      ro.disconnect();
      window.removeEventListener("resize", onWinResize);
      window.removeEventListener("keydown", onKey);
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [scheduleFit]);

  // Live-apply font size and color scheme without tearing down the session.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = prefs.fontSize;
    term.options.theme = THEMES[prefs.themeId].theme;
    savePrefs(prefs);
    scheduleFit();
  }, [prefs, scheduleFit]);

  // Refit when chrome visibility changes (detail panel / bar show·hide).
  useEffect(() => {
    scheduleFit();
  }, [terminalOnly, scheduleFit]);

  // WebSocket with automatic reconnect. Terminal stays up across reconnects;
  // the server re-sends a backlog snapshot on each new subscribe.
  useEffect(() => {
    intentionalCloseRef.current = false;
    reconnectAttemptRef.current = 0;
    clearReconnectTimer();

    const connect = (isRetry: boolean) => {
      if (intentionalCloseRef.current) return;

      const prev = socketRef.current;
      if (prev) {
        socketRef.current = null;
        prev.onopen = null;
        prev.onmessage = null;
        prev.onerror = null;
        prev.onclose = null;
        prev.close();
      }

      setStatus(isRetry ? "reconnecting" : "connecting");

      const socket = new WebSocket(consoleWsUrl(vmId));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;

      socket.onopen = () => {
        reconnectAttemptRef.current = 0;
        setStatus("connected");
        termRef.current?.focus();
        scheduleFit();
      };

      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        const term = termRef.current;
        if (!term) return;
        term.write(typeof event.data === "string" ? event.data : new Uint8Array(event.data));
      };

      socket.onerror = () => {
        if (socket.readyState !== WebSocket.OPEN && reconnectAttemptRef.current === 0) {
          setStatus("failed");
        }
      };

      socket.onclose = () => {
        if (socketRef.current === socket) {
          socketRef.current = null;
        }
        if (intentionalCloseRef.current) return;
        setStatus("disconnected");
        scheduleReconnect();
      };
    };

    const scheduleReconnect = () => {
      if (intentionalCloseRef.current) return;
      clearReconnectTimer();
      const attempt = reconnectAttemptRef.current;
      const delay = Math.min(RECONNECT_BASE_MS * 2 ** attempt, RECONNECT_MAX_MS);
      reconnectAttemptRef.current = attempt + 1;
      setStatus("reconnecting");
      reconnectTimerRef.current = setTimeout(() => connect(true), delay);
    };

    connect(false);

    return () => {
      intentionalCloseRef.current = true;
      clearReconnectTimer();
      const socket = socketRef.current;
      if (socket) {
        socketRef.current = null;
        socket.onopen = null;
        socket.onmessage = null;
        socket.onerror = null;
        socket.onclose = null;
        socket.close();
      }
    };
  }, [vmId, reconnectKey, clearReconnectTimer, scheduleFit]);

  const reconnectNow = () => {
    clearReconnectTimer();
    reconnectAttemptRef.current = 0;
    setReconnectKey((k) => k + 1);
  };

  /**
   * Prefer closing a script-opened popup (`window.open` from the list).
   * If the browser refuses (normal tab / no script opener), fall back to
   * navigating back to the VM list in this tab.
   */
  const handleClose = () => {
    window.close();
    window.setTimeout(() => {
      if (!window.closed) onClose();
    }, 50);
  };

  const canRetry = status === "disconnected" || status === "failed" || status === "reconnecting";
  const title = vm ? `terminal — ${vm.name}` : `terminal — ${vmId.slice(0, 8)}`;

  return (
    <div
      className={`console-page${terminalOnly ? " is-terminal-only" : ""}`}
      ref={pageRef}
    >
      {!terminalOnly && (
        <div className="console-bar">
          <button
            type="button"
            className="btn console-bar-btn"
            onClick={handleClose}
            title="창 닫기"
          >
            창 닫기
          </button>

          <span className="console-title" title={vmId}>
            {title}
          </span>

          <span
            className={`console-status ${STATUS_CLASS[status]}`}
            role="status"
            aria-live="polite"
          >
            <span className="console-status-dot" aria-hidden />
            {STATUS_LABEL[status]}
          </span>

          {canRetry && (
            <button
              type="button"
              className="btn console-bar-btn"
              onClick={reconnectNow}
              title="지금 다시 연결"
            >
              재연결
            </button>
          )}

          <div className="console-settings">
            <button
              type="button"
              className={`btn console-bar-btn${settingsOpen ? " is-active" : ""}`}
              onClick={() => setSettingsOpen((o) => !o)}
              aria-expanded={settingsOpen}
              aria-haspopup="true"
              title="폰트 · 색상"
            >
              표시
            </button>
            {settingsOpen && (
              <div className="console-settings-pop" role="dialog" aria-label="터미널 표시 설정">
                <label className="console-settings-row">
                  <span>글자 크기</span>
                  <select
                    value={prefs.fontSize}
                    onChange={(e) => setPrefs((p) => ({ ...p, fontSize: Number(e.target.value) }))}
                  >
                    {FONT_SIZES.map((n) => (
                      <option key={n} value={n}>
                        {n}px
                      </option>
                    ))}
                  </select>
                </label>
                <label className="console-settings-row">
                  <span>색 테마</span>
                  <select
                    value={prefs.themeId}
                    onChange={(e) =>
                      setPrefs((p) => ({ ...p, themeId: e.target.value as ThemeId }))
                    }
                  >
                    {Object.entries(THEMES).map(([id, { label }]) => (
                      <option key={id} value={id}>
                        {label}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
            )}
          </div>

          <button
            type="button"
            className="btn console-bar-btn"
            onClick={() => {
              setTerminalOnly(true);
              setSettingsOpen(false);
            }}
            title="툴바·세부정보를 숨기고 터미널만 표시 (Esc로 복귀)"
          >
            터미널만
          </button>

          <button type="button" className="btn console-close" onClick={handleClose} title="닫기">
            ✕
          </button>
        </div>
      )}

      <div
        className="console-surface"
        ref={containerRef}
        onClick={() => {
          setSettingsOpen(false);
          termRef.current?.focus();
        }}
      />

      {/* Floating control when chrome is hidden — otherwise Esc is the only exit. */}
      {terminalOnly && (
        <button
          type="button"
          className="console-exit-only"
          onClick={() => setTerminalOnly(false)}
          title="툴바·세부정보 다시 표시 (Esc)"
        >
          세부정보 보기
        </button>
      )}

      {!terminalOnly && (
        <aside className="console-detail" aria-label="VM 세부정보">
          <div className="console-detail-head">
            <h2 className="console-detail-title">세부정보</h2>
            {vm && <span className={`state-badge ${vm.state}`}>{vm.state}</span>}
            <button
              type="button"
              className="btn console-bar-btn"
              onClick={() => setTerminalOnly(true)}
              title="터미널만 보기"
            >
              터미널만
            </button>
          </div>
          {vmError && !vm ? (
            <p className="console-detail-error">{vmError}</p>
          ) : vm ? (
            <div className="console-detail-scroll">
              <table className="console-detail-table">
                <thead>
                  <tr>
                    <th>name</th>
                    <th>id</th>
                    <th>template</th>
                    <th>cpu</th>
                    <th>ram</th>
                    <th>disk</th>
                    <th>hostname</th>
                    <th>ipv4</th>
                    <th>mac</th>
                    <th>egress</th>
                    <th>network</th>
                    <th>storage</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td>{vm.name}</td>
                    <td className="mono" title={vm.id}>
                      {vm.id}
                    </td>
                    <td className="mono">{vm.templateVersion}</td>
                    <td className="mono">{vm.cpu}</td>
                    <td className="mono">{vm.ram} MiB</td>
                    <td className="mono">{vm.diskGb} GiB</td>
                    <td className="mono">{vm.hostname}</td>
                    <td className="mono">{vm.ipv4 ?? "—"}</td>
                    <td className="mono">{vm.mac ?? "—"}</td>
                    <td>{EGRESS_LABEL[vm.egressPolicy] ?? vm.egressPolicy}</td>
                    <td className="mono" title={vm.microNetworkId}>
                      {vm.microNetworkId}
                    </td>
                    <td className="mono">{vm.storageRoot || "default"}</td>
                  </tr>
                </tbody>
              </table>
            </div>
          ) : (
            <p className="console-detail-loading">불러오는 중…</p>
          )}
        </aside>
      )}
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
