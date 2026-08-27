import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { VmResponse } from "../bindings";
import { getVm, getVmLog } from "../api/client";
import { formatVmExportBundle, serializeXtermBuffer } from "../lib/formatVmLog";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";
import ConsoleSshTab from "./ConsoleSshTab";
import UsageCharts from "./UsageCharts";
import { useI18n } from "../i18n";
import {
  defaultInteractiveTerminalOptions,
  enableTerminalIme,
  isImeComposing,
} from "../lib/terminal";

type Status = "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";

const STATUS_CLASS: Record<Status, string> = {
  connecting: "connecting",
  connected: "connected",
  reconnecting: "connecting",
  disconnected: "error",
  failed: "error",
};

/** Named color schemes. Font stack is shared in `lib/terminal.ts` (CJK fallbacks). */
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
  inspectOpen: boolean;
}

const DEFAULT_PREFS: ConsolePrefs = { fontSize: 13, themeId: "ember", inspectOpen: true };

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
    const inspectOpen = typeof parsed.inspectOpen === "boolean" ? parsed.inspectOpen : true;
    return { fontSize, themeId, inspectOpen };
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
  const { t } = useI18n();
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
  const [surfaceTab, setSurfaceTab] = useState<"serial" | "ssh">("serial");
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

  /** Fit after layout paints; retry a few times if the box is still empty. */
  const scheduleFit = useCallback(() => {
    let tries = 0;
    const tick = () => {
      if (doFit()) return;
      tries += 1;
      if (tries < 20) {
        requestAnimationFrame(tick);
      }
    };
    requestAnimationFrame(() => requestAnimationFrame(tick));
  }, [doFit]);

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

  // Terminal + WebSocket share one effect so StrictMode remount never leaves
  // a live socket writing into a disposed Terminal (blank screen).
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    intentionalCloseRef.current = false;
    reconnectAttemptRef.current = 0;

    const initial = initialPrefsRef.current;
    const term = new Terminal(
      defaultInteractiveTerminalOptions({
        fontSize: initial.fontSize,
        theme: THEMES[initial.themeId].theme,
        // Avoid 0×0 first paint — FitAddon replaces these once the box is ready.
        cols: 80,
        rows: 24,
      }),
    );
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);
    enableTerminalIme(term, container);
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

    const ro = new ResizeObserver(() => {
      if (!disposed) scheduleFit();
    });
    ro.observe(container);
    const page = pageRef.current;
    if (page) ro.observe(page);

    const onWinResize = () => {
      if (!disposed) scheduleFit();
    };
    window.addEventListener("resize", onWinResize);

    const onKey = (event: KeyboardEvent) => {
      if (isImeComposing(event)) return;
      if (event.key !== "Escape" || event.ctrlKey || event.metaKey || event.altKey) return;
      if (!terminalOnlyRef.current) return;
      event.preventDefault();
      setTerminalOnly(false);
    };
    window.addEventListener("keydown", onKey);

    const connect = (isRetry: boolean) => {
      if (disposed || intentionalCloseRef.current) return;

      const prev = socketRef.current;
      if (prev) {
        socketRef.current = null;
        prev.onopen = null;
        prev.onmessage = null;
        prev.onerror = null;
        prev.onclose = null;
        try {
          prev.close();
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
        term.focus();
      };

      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        if (disposed || socketRef.current !== socket) return;
        const live = termRef.current;
        if (!live) return;
        live.write(typeof event.data === "string" ? event.data : new Uint8Array(event.data));
      };

      socket.onerror = () => {
        if (disposed || socketRef.current !== socket) return;
        if (socket.readyState !== WebSocket.OPEN && reconnectAttemptRef.current === 0) {
          setStatus("failed");
        }
      };

      socket.onclose = () => {
        if (socketRef.current === socket) {
          socketRef.current = null;
        }
        if (disposed || intentionalCloseRef.current) return;
        setStatus("disconnected");
        scheduleReconnect();
      };
    };

    const scheduleReconnect = () => {
      if (disposed || intentionalCloseRef.current) return;
      clearReconnectTimer();
      const attempt = reconnectAttemptRef.current;
      const delay = Math.min(RECONNECT_BASE_MS * 2 ** attempt, RECONNECT_MAX_MS);
      reconnectAttemptRef.current = attempt + 1;
      setStatus("reconnecting");
      reconnectTimerRef.current = setTimeout(() => connect(true), delay);
    };

    // Defer the first WS connect one frame so the terminal has painted and
    // fit() can run before the backlog snapshot arrives.
    const bootTimer = window.setTimeout(() => {
      if (!disposed) connect(false);
    }, 0);

    return () => {
      disposed = true;
      intentionalCloseRef.current = true;
      window.clearTimeout(bootTimer);
      clearReconnectTimer();
      dataListener.dispose();
      ro.disconnect();
      window.removeEventListener("resize", onWinResize);
      window.removeEventListener("keydown", onKey);
      const socket = socketRef.current;
      if (socket) {
        socketRef.current = null;
        socket.onopen = null;
        socket.onmessage = null;
        socket.onerror = null;
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
  }, [vmId, reconnectKey, clearReconnectTimer, scheduleFit]);

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
  const inspectOpen = prefs.inspectOpen;
  useEffect(() => {
    scheduleFit();
  }, [terminalOnly, surfaceTab, inspectOpen, scheduleFit]);

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
  const sessionName = vm?.name ?? vmId.slice(0, 8);

  /** Server console.log + startup timeline + live xterm buffer for copy/download. */
  const buildExportText = useCallback(async () => {
    let consoleLog = "";
    try {
      const log = await getVmLog(vmId);
      consoleLog = log.consoleLog;
    } catch {
      consoleLog = "";
    }
    const term = termRef.current;
    const liveTerminal = term ? serializeXtermBuffer(term) : undefined;
    return formatVmExportBundle({
      vm,
      vmId,
      consoleLog,
      liveTerminal,
    });
  }, [vm, vmId]);

  return (
    <div
      className={`console-page${terminalOnly ? " is-terminal-only" : ""}`}
      ref={pageRef}
    >
      {!terminalOnly && (
        <div className="console-bar" aria-label={`terminal — ${sessionName}`}>
          <span
            className={`console-status ${STATUS_CLASS[status]}`}
            role="status"
            aria-live="polite"
          >
            <span className="console-status-dot" aria-hidden />
            {status === "connecting"
              ? t("Connecting…", "연결 중…")
              : status === "connected"
                ? t("Connected", "연결됨")
                : status === "reconnecting"
                  ? t("Reconnecting…", "재연결 중…")
                  : status === "disconnected"
                    ? t("Disconnected", "연결 끊김")
                    : t("Connection failed", "연결 실패")}
          </span>

          <span className="console-session" title={vmId}>
            <span className="console-session-caret" aria-hidden>
              ▍
            </span>
            <span className="console-session-name">{sessionName}</span>
            {vm?.hostname ? (
              <span className="console-session-host">{vm.hostname}</span>
            ) : null}
            {vm?.ipv4 ? <span className="console-session-addr">{vm.ipv4}</span> : null}
          </span>

          {canRetry && (
            <button
              type="button"
              className="btn console-bar-btn"
              onClick={reconnectNow}
              title={t("Reconnect now", "지금 다시 연결")}
            >
              {t("Reconnect", "재연결")}
            </button>
          )}

          <div className="console-bar-actions">
            <div className="console-tabs" role="tablist" aria-label={t("Console surface", "콘솔 화면")}>
              <button
                type="button"
                role="tab"
                aria-selected={surfaceTab === "serial"}
                className={`btn console-bar-btn${surfaceTab === "serial" ? " is-active" : ""}`}
                onClick={() => setSurfaceTab("serial")}
              >
                {t("Serial", "시리얼")}
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={surfaceTab === "ssh"}
                className={`btn console-bar-btn${surfaceTab === "ssh" ? " is-active" : ""}`}
                onClick={() => setSurfaceTab("ssh")}
              >
                SSH
              </button>
            </div>

            <div className="console-settings">
              <button
                type="button"
                className={`btn console-bar-btn${settingsOpen ? " is-active" : ""}`}
                onClick={() => setSettingsOpen((o) => !o)}
                aria-expanded={settingsOpen}
                aria-haspopup="true"
                title={t("Font · colors", "폰트 · 색상")}
              >
                {t("Display", "표시")}
              </button>
              {settingsOpen && (
                <div className="console-settings-pop" role="dialog" aria-label={t("Terminal display settings", "터미널 표시 설정")}>
                  <label className="console-settings-row">
                    <span>{t("Font size", "글자 크기")}</span>
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
                    <span>{t("Color theme", "색 테마")}</span>
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

            <LogExportActions
              text={buildExportText}
              filename={logDownloadFilename("console", vm?.name ?? vmId)}
              buttonClassName="btn console-bar-btn"
              copyLabel={t("Copy log", "로그 복사")}
              downloadLabel={t("Save log", "로그 저장")}
            />
            <button
              type="button"
              className="btn console-bar-btn"
              onClick={() => {
                setTerminalOnly(true);
                setSettingsOpen(false);
              }}
              title={t("Hide the toolbar and details; show terminal only (Esc to return)", "툴바·세부정보를 숨기고 터미널만 표시 (Esc로 복귀)")}
            >
              {t("Terminal only", "터미널만")}
            </button>

            <button type="button" className="btn console-close" onClick={handleClose} title={t("Close", "닫기")}>
              {t("Close", "닫기")}
            </button>
          </div>
        </div>
      )}

      <div
        className="console-surface"
        hidden={surfaceTab !== "serial"}
        ref={containerRef}
        onClick={() => {
          setSettingsOpen(false);
          termRef.current?.focus();
        }}
      />
      {surfaceTab === "ssh" ? <ConsoleSshTab vm={vm} /> : null}

      {/* Floating control when chrome is hidden — otherwise Esc is the only exit. */}
      {terminalOnly && (
        <button
          type="button"
          className="console-exit-only"
          onClick={() => setTerminalOnly(false)}
          title={t("Show toolbar and details (Esc)", "툴바·세부정보 다시 표시 (Esc)")}
        >
          {t("Show details", "세부정보 보기")}
        </button>
      )}

      {!terminalOnly && prefs.inspectOpen && (
        <aside
          id="console-inspect-rail"
          className="console-detail"
          aria-label={t("VM details", "VM 세부정보")}
        >
          {vmError && !vm ? (
            <p className="console-detail-error">{vmError}</p>
          ) : vm ? (
            <div className="console-detail-grid">
              <section className="console-detail-group" aria-label="기본">
                <h3 className="console-detail-group-title">{t("General", "기본")}</h3>
                <dl className="console-detail-fields mono">
                  <dt>name</dt>
                  <dd>{vm.name}</dd>
                  <dt>id</dt>
                  <dd title={vm.id}>{vm.id}</dd>
                  <dt>image</dt>
                  <dd>{vm.template}</dd>
                  <dt>hostname</dt>
                  <dd>{vm.hostname}</dd>
                </dl>
              </section>
              <section className="console-detail-group" aria-label="스펙">
                <h3 className="console-detail-group-title">{t("Specs", "스펙")}</h3>
                <dl className="console-detail-fields mono">
                  <dt>cpu</dt>
                  <dd>{vm.cpu}</dd>
                  <dt>ram</dt>
                  <dd>{vm.ram} MiB</dd>
                  <dt>disk</dt>
                  <dd>{vm.diskGb} GiB</dd>
                </dl>
              </section>
              <section className="console-detail-group" aria-label="네트워크">
                <h3 className="console-detail-group-title">{t("Network", "네트워크")}</h3>
                <dl className="console-detail-fields mono">
                  <dt>ipv4</dt>
                  <dd>{vm.ipv4 ?? "—"}</dd>
                  <dt>ipv6</dt>
                  <dd>{vm.ipv6 ?? "—"}</dd>
                  <dt>mac</dt>
                  <dd>{vm.mac ?? "—"}</dd>
                  <dt>egress</dt>
                  <dd>{vm.egressPolicy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated", "격리")}</dd>
                  <dt>network</dt>
                  <dd title={vm.microNetworkId}>{vm.microNetworkId.slice(0, 8)}</dd>
                </dl>
              </section>
              <section
                className="console-detail-group console-detail-usage-group"
                aria-label={t("Resource usage", "리소스 관측")}
              >
                <h3 className="console-detail-group-title">
                  {t("Resource usage", "리소스 관측")}
                </h3>
                {vm.state === "running" && (vm.usageHistory?.length ?? 0) > 0 ? (
                  <UsageCharts
                    history={vm.usageHistory ?? []}
                    ramMib={vm.ram}
                    size="compact"
                  />
                ) : (
                  <p className="console-detail-empty">
                    {vm.state === "running"
                      ? t("Collecting samples…", "샘플 수집 중…")
                      : t("Available while running", "running일 때 표시")}
                  </p>
                )}
              </section>
              <section className="console-detail-group console-port-forward-group" aria-label="포트 포워딩">
                <h3 className="console-detail-group-title">{t("Port Forwarding", "포트 포워딩")}</h3>
                {(vm.portForwards?.length ?? 0) > 0 ? (
                  <dl className="console-detail-fields console-port-forward-list mono">
                    {vm.portForwards!.map((pf, index) => (
                      <div key={`${pf.protocol}-${pf.hostPort}-${pf.guestPort}-${index}`} style={{ display: "contents" }}>
                        <dt>{String(index + 1).padStart(2, "0")}</dt>
                        <dd>
                          :{pf.guestPort} <span className="console-pf-arrow">→</span> :{pf.hostPort}/{pf.protocol}
                        </dd>
                      </div>
                    ))}
                  </dl>
                ) : (
                  <p className="console-detail-empty">{t("No active port forwarding rules.", "설정된 포트 포워딩 규칙이 없습니다.")}</p>
                )}
              </section>
              <section className="console-detail-group console-storage-group" aria-label="스토리지">
                <h3 className="console-detail-group-title">{t("Storage", "스토리지")}</h3>
                <dl className="console-detail-fields mono">
                  <dt>storage</dt>
                  <dd>{vm.storageRoot || "default"}</dd>
                </dl>
              </section>
            </div>
          ) : (
            <p className="console-detail-loading">{t("Loading…", "불러오는 중…")}</p>
          )}
        </aside>
      )}

      {!terminalOnly && (
        <button
          type="button"
          className={`console-inspect-bar${prefs.inspectOpen ? " is-open" : ""}`}
          aria-expanded={prefs.inspectOpen}
          aria-controls="console-inspect-rail"
          onClick={() => setPrefs((p) => ({ ...p, inspectOpen: !p.inspectOpen }))}
          title={
            prefs.inspectOpen
              ? t("Hide inspect", "inspect 숨기기")
              : t("Show inspect", "inspect 보기")
          }
        >
          <span className="console-inspect-chevron" aria-hidden>
            {prefs.inspectOpen ? "▾" : "▴"}
          </span>
          <span className="console-inspect-bar-label">{t("inspect", "inspect")}</span>
          {vm ? <span className={`state-badge ${vm.state}`}>{vm.state}</span> : null}
        </button>
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
