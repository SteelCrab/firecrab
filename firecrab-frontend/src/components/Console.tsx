import { useCallback, useEffect, useRef, useState } from "react";
import { Maximize2, PanelRight, PanelRightClose, SlidersHorizontal, SquareTerminal, X } from "lucide-react";
import type { ITheme, Terminal } from "ghostty-web";
import { createWasmTerminal, type WasmTerminalSession } from "../lib/wasmTerminal";
import type { VmResponse } from "../bindings";
import { getVm, getVmLog } from "../api/client";
import { formatVmExportBundle } from "../lib/formatVmLog";
import { logDownloadFilename } from "../lib/textExport";
import LogExportActions from "./LogExportActions";
import SshAccess from "./ssh/SshAccess";
import { DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";
import VmUsage from "./vm/VmUsage";
import { useI18n } from "../i18n";
import {
  isImeComposing,
} from "../lib/terminal";

type Status = "connecting" | "connected" | "reconnecting" | "disconnected" | "failed" | "ended";

const STATUS_CLASS: Record<Status, string> = {
  connecting: "connecting",
  connected: "connected",
  reconnecting: "connecting",
  disconnected: "error",
  failed: "error",
  ended: "ended",
};

/**
 * Close code the API sends when the guest's login session ended (`exit`).
 * Mirrors `CONSOLE_SESSION_ENDED_CLOSE_CODE` in `firecrab-api-types`.
 */
const SESSION_ENDED_CLOSE_CODE = 4000;

/** Named color schemes. Font stack is shared in `lib/terminal.ts` (CJK fallbacks). */
const THEMES: Record<string, { label: string; theme: ITheme }> = {
  ember: {
    label: "Firecrab",
    theme: {
      background: "#18181b",
      foreground: "#e4e4e7",
      cursor: "#e8492f",
      selectionBackground: "rgba(232, 73, 47, 0.32)",
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
      background: "#fafafb",
      foreground: "#18181b",
      cursor: "#e8492f",
      selectionBackground: "rgba(232, 73, 47, 0.2)",
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
 * Layout: header, then the terminal (or the SSH steps) beside a VM details
 * column. "Terminal only" hides the header and details so only the terminal
 * remains.
 */
export default function Console({ vmId, onClose }: ConsoleProps) {
  const { t } = useI18n();
  const containerRef = useRef<HTMLDivElement>(null);
  const pageRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const outputRef = useRef<HTMLPreElement>(null);
  const sessionRef = useRef<WasmTerminalSession | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const reconnectAttemptRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const [status, setStatus] = useState<Status>("connecting");
  const [prefs, setPrefs] = useState<ConsolePrefs>(loadPrefs);
  const [appliedThemeId, setAppliedThemeId] = useState<ThemeId>(prefs.themeId);
  const prefsRef = useRef(prefs);
  const [terminalError, setTerminalError] = useState<string | null>(null);
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
  /** Written into the terminal when the guest session ends; a ref so a language switch does not reconnect. */
  const sessionEndedNoticeRef = useRef("");
  const sessionEndedNotice = t(
    "Guest session ended. Use New session to open another.",
    "게스트 세션이 종료되었습니다. 새 세션으로 다시 열 수 있습니다.",
  );
  useEffect(() => {
    sessionEndedNoticeRef.current = sessionEndedNotice;
  }, [sessionEndedNotice]);

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  const scheduleFit = useCallback(() => sessionRef.current?.fit(), []);

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

  // An aborted mount owns no socket, WASM terminal or scheduled callbacks.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const controller = new AbortController();
    const { signal } = controller;
    let session: WasmTerminalSession | undefined;
    let bootTimer: ReturnType<typeof setTimeout> | undefined;
    reconnectAttemptRef.current = 0;
    setTerminalError(null);
    setStatus("connecting");
    if (outputRef.current) outputRef.current.textContent = "";

    const disconnect = () => {
      const socket = socketRef.current;
      socketRef.current = null;
      if (!socket) return;
      socket.onopen = null;
      socket.onmessage = null;
      socket.onerror = null;
      socket.onclose = null;
      socket.close();
    };
    const cleanup = () => {
      controller.abort();
      clearTimeout(bootTimer);
      clearReconnectTimer();
      disconnect();
      session?.dispose();
      if (termRef.current === session?.terminal) termRef.current = null;
      if (sessionRef.current === session) sessionRef.current = null;
    };
    const fail = (error: unknown) => {
      if (signal.aborted) return;
      cleanup();
      setStatus("failed");
      setTerminalError(error instanceof Error ? error.message : String(error));
    };
    const connect = (isRetry = false) => {
      if (signal.aborted || !session) return;
      disconnect();
      const { terminal } = session;
      terminal.options.disableStdin = true;
      setStatus(isRetry ? "reconnecting" : "connecting");
      const socket = new WebSocket(consoleWsUrl(vmId));
      socket.binaryType = "arraybuffer";
      socketRef.current = socket;
      socket.onopen = () => {
        if (signal.aborted || socketRef.current !== socket) return;
        reconnectAttemptRef.current = 0;
        terminal.options.disableStdin = false;
        setStatus("connected");
        session?.fit();
        terminal.focus();
      };
      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        if (signal.aborted || socketRef.current !== socket) return;
        session?.write(typeof event.data === "string" ? event.data : new Uint8Array(event.data));
      };
      socket.onerror = () => {
        if (!signal.aborted && socketRef.current === socket) setStatus("failed");
      };
      socket.onclose = (event: CloseEvent) => {
        if (signal.aborted || socketRef.current !== socket) return;
        socketRef.current = null;
        terminal.options.disableStdin = true;
        if (event.code === SESSION_ENDED_CLOSE_CODE) {
          setStatus("ended");
          session?.write(`\r\n\x1b[2m${sessionEndedNoticeRef.current}\x1b[0m\r\n`);
          return;
        }
        clearReconnectTimer();
        const delay = Math.min(RECONNECT_BASE_MS * 2 ** reconnectAttemptRef.current++, RECONNECT_MAX_MS);
        setStatus("reconnecting");
        reconnectTimerRef.current = setTimeout(() => connect(true), delay);
      };
    };

    const initialize = async () => {
      const encoder = new TextEncoder();
      const initial = prefsRef.current;
      session = await createWasmTerminal(container, {
        fontSize: initial.fontSize, theme: THEMES[initial.themeId].theme,
        cols: 80, rows: 24, disableStdin: true,
      }, signal, text => {
        if (outputRef.current) outputRef.current.textContent = text;
      }, fail, data => {
        // The API removes queries from replay; live VT replies reach the guest.
        const socket = socketRef.current;
        if (socket?.readyState === WebSocket.OPEN) socket.send(typeof data === "string" ? encoder.encode(data) : Uint8Array.from(data));
      });
      if (signal.aborted) { session.dispose(); return; }
      const { terminal } = session;
      const latest = prefsRef.current;
      terminal.options.fontSize = latest.fontSize;
      setAppliedThemeId(initial.themeId);
      termRef.current = terminal;
      sessionRef.current = session;
      session.fit();
      window.addEventListener("keydown", event => {
        if (isImeComposing(event) || event.key !== "Escape" || event.ctrlKey || event.metaKey || event.altKey) return;
        setSettingsOpen(false);
        if (!terminalOnlyRef.current) return;
        event.preventDefault();
        setTerminalOnly(false);
      }, { signal });
      bootTimer = setTimeout(() => connect(), 0);
    };
    void initialize().catch(fail);
    return cleanup;
  }, [vmId, reconnectKey, clearReconnectTimer]);

  // Font changes are live. The native core applies a new theme on reconnect.
  useEffect(() => {
    prefsRef.current = prefs;
    savePrefs(prefs);
    const term = termRef.current;
    if (!term) return;
    term.options.fontSize = prefs.fontSize;
    if (term.element) term.element.style.fontSize = `${prefs.fontSize}px`;
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

  const canRetry =
    status === "disconnected" || status === "failed" || status === "reconnecting" || status === "ended";
  const sessionName = vm?.name ?? vmId.slice(0, 8);

  /** Server console.log + startup timeline + the drained WASM session for export. */
  const buildExportText = useCallback(async () => {
    let consoleLog = "";
    try {
      const log = await getVmLog(vmId);
      consoleLog = log.consoleLog;
    } catch {
      consoleLog = "";
    }
    const liveTerminal = await sessionRef.current?.readText();
    return formatVmExportBundle({
      vm,
      vmId,
      consoleLog,
      liveTerminal,
    });
  }, [vm, vmId]);

  const statusLabel =
    status === "connecting" ? t("Connecting…", "연결 중…")
    : status === "connected" ? t("Connected", "연결됨")
    : status === "reconnecting" ? t("Reconnecting…", "재연결 중…")
    : status === "disconnected" ? t("Disconnected", "연결 끊김")
    : status === "ended" ? t("Session ended", "세션 종료됨")
    : t("Connection failed", "연결 실패");
  const detailsLabel = inspectOpen ? t("Hide details", "세부정보 숨기기") : t("Show details", "세부정보 보기");
  const showDetails = !terminalOnly && inspectOpen;

  return (
    <div className={`console-page${terminalOnly ? " is-terminal-only" : ""}`} ref={pageRef}>
      {!terminalOnly && (
        <header className="console-header" aria-label={`terminal — ${sessionName}`}>
          <span className="console-brand" aria-hidden="true"><SquareTerminal /></span>
          <div className="console-heading" title={vmId}>
            <h1 className="console-title">{sessionName}</h1>
            {vm?.hostname && vm.hostname !== vm.name && <span className="console-host">{vm.hostname}</span>}
            {vm?.ipv4 && <span className="console-addr">{vm.ipv4}</span>}
          </div>
          <span className={`console-status ${STATUS_CLASS[status]}`} role="status" aria-live="polite">
            <span className="console-status-dot" aria-hidden="true" />
            {statusLabel}
          </span>

          <div className="segmented console-tabs" role="tablist" aria-label={t("Console surface", "콘솔 화면")}>
            <button type="button" role="tab" aria-selected={surfaceTab === "serial"} onClick={() => setSurfaceTab("serial")}>
              {t("Serial", "시리얼")}
            </button>
            <button type="button" role="tab" aria-selected={surfaceTab === "ssh"} onClick={() => setSurfaceTab("ssh")}>
              SSH
            </button>
          </div>

          <div className="console-header-end">
            {canRetry && (
              <button
                type="button"
                className="btn sm"
                onClick={reconnectNow}
                title={status === "ended" ? t("Attach to the guest's new shell", "게스트의 새 셸에 연결") : t("Reconnect now", "지금 다시 연결")}
              >
                {status === "ended" ? t("New session", "새 세션") : t("Reconnect", "재연결")}
              </button>
            )}
            <LogExportActions
              text={buildExportText}
              filename={logDownloadFilename("console", vm?.name ?? vmId)}
              buttonClassName="btn sm ghost"
              copyLabel={t("Copy log", "로그 복사")}
              downloadLabel={t("Save log", "로그 저장")}
            />
            <div className="console-settings">
              <button
                type="button"
                className="icon-btn"
                aria-label={t("Display settings", "표시 설정")}
                title={t("Font · colors", "폰트 · 색상")}
                aria-expanded={settingsOpen}
                aria-haspopup="dialog"
                onClick={() => setSettingsOpen((open) => !open)}
              >
                <SlidersHorizontal aria-hidden="true" />
              </button>
              {settingsOpen && (
                <div className="console-settings-pop" role="dialog" aria-label={t("Terminal display settings", "터미널 표시 설정")}>
                  <label className="console-settings-row">
                    <span>{t("Font size", "글자 크기")}</span>
                    <select className="input" value={prefs.fontSize} onChange={(e) => setPrefs((p) => ({ ...p, fontSize: Number(e.target.value) }))}>
                      {FONT_SIZES.map((n) => <option key={n} value={n}>{n}px</option>)}
                    </select>
                  </label>
                  <label className="console-settings-row">
                    <span>{t("Color theme", "색 테마")}</span>
                    <select className="input" value={prefs.themeId} onChange={(e) => setPrefs((p) => ({ ...p, themeId: e.target.value as ThemeId }))}>
                      {Object.entries(THEMES).map(([id, { label }]) => <option key={id} value={id}>{label}</option>)}
                    </select>
                  </label>
                  {prefs.themeId !== appliedThemeId && (
                    <button type="button" className="btn sm" onClick={() => { setSettingsOpen(false); reconnectNow(); }}>
                      {t("Apply theme and reconnect", "테마 적용 후 재연결")}
                    </button>
                  )}
                </div>
              )}
            </div>
            <button
              type="button"
              className="icon-btn"
              aria-label={t("Terminal only", "터미널만")}
              title={t("Hide the header and details; show the terminal only (Esc to return)", "헤더·세부정보를 숨기고 터미널만 표시 (Esc로 복귀)")}
              onClick={() => {
                setTerminalOnly(true);
                setSettingsOpen(false);
              }}
            >
              <Maximize2 aria-hidden="true" />
            </button>
            <button
              type="button"
              className="icon-btn"
              aria-label={detailsLabel}
              title={detailsLabel}
              aria-expanded={inspectOpen}
              aria-controls="console-inspect-rail"
              onClick={() => setPrefs((p) => ({ ...p, inspectOpen: !p.inspectOpen }))}
            >
              {inspectOpen ? <PanelRightClose aria-hidden="true" /> : <PanelRight aria-hidden="true" />}
            </button>
            <span className="console-divider" aria-hidden="true" />
            <button type="button" className="icon-btn console-close" aria-label={t("Close", "닫기")} title={t("Close", "닫기")} onClick={handleClose}>
              <X aria-hidden="true" />
            </button>
          </div>
        </header>
      )}

      <div className={showDetails ? "console-body" : "console-body no-details"}>
        <div className="console-main">
          <div
            className="console-surface"
            hidden={surfaceTab !== "serial"}
            ref={containerRef}
            style={{ background: THEMES[appliedThemeId].theme.background }}
            onClick={() => {
              setSettingsOpen(false);
              termRef.current?.focus();
            }}
          />
          <pre ref={outputRef} className="terminal-output" aria-label={t("Terminal output", "터미널 출력")} aria-live="polite" aria-atomic="true" />
          {terminalError && <p role="alert" className="console-detail-note">{terminalError}</p>}
          {surfaceTab === "ssh" && (
            <div className="console-ssh-pane">
              <SshAccess vm={vm} />
            </div>
          )}
        </div>

        {showDetails && (
          <aside id="console-inspect-rail" className="console-detail" aria-label={t("VM details", "VM 세부정보")}>
            {vmError && !vm ? (
              <p className="console-detail-note error">{vmError}</p>
            ) : vm ? (
              <ConsoleDetails vm={vm} />
            ) : (
              <p className="console-detail-note">{t("Loading…", "불러오는 중…")}</p>
            )}
          </aside>
        )}
      </div>

      {/* Floating control when the chrome is hidden — otherwise Esc is the only exit. */}
      {terminalOnly && (
        <button
          type="button"
          className="btn sm console-exit-only"
          onClick={() => setTerminalOnly(false)}
          title={t("Show the header and details (Esc)", "헤더·세부정보 다시 표시 (Esc)")}
        >
          {t("Exit terminal only", "터미널만 끝내기")}
        </button>
      )}
    </div>
  );
}

/** The VM beside its terminal: what it is, what it uses, how it is reached. */
function ConsoleDetails({ vm }: { vm: VmResponse }) {
  const { t } = useI18n();
  const forwards = vm.portForwards ?? [];
  return (
    <>
      <DetailSection title={t("Overview", "개요")} aside={<StatusBadge state={vm.state} />}>
        <dl className="props">
          <DetailProp label={t("Image", "이미지")}>{vm.template}</DetailProp>
          <DetailProp label={t("Hostname", "호스트명")}>{vm.hostname}</DetailProp>
          <DetailProp label="ID" copy={vm.id}><span className="mono">{vm.id}</span></DetailProp>
        </dl>
      </DetailSection>
      <DetailSection title={t("Resources", "리소스")}>
        <dl className="props">
          <DetailProp label="vCPU">{vm.cpu}</DetailProp>
          <DetailProp label={t("Memory", "메모리")}>{vm.ram} MiB</DetailProp>
          <DetailProp label={t("Disk", "디스크")}>{vm.diskGb} GiB</DetailProp>
        </dl>
        <VmUsage vm={vm} />
      </DetailSection>
      <DetailSection title={t("Network", "네트워크")}>
        <dl className="props">
          <DetailProp label="IPv4" copy={vm.ipv4 ?? undefined}><span className="mono">{vm.ipv4 ?? "—"}</span></DetailProp>
          <DetailProp label="IPv6" copy={vm.ipv6 ?? undefined}><span className="mono">{vm.ipv6 ?? "—"}</span></DetailProp>
          <DetailProp label="MAC"><span className="mono">{vm.mac ?? "—"}</span></DetailProp>
          <DetailProp label={t("Egress", "외부 통신")}>{vm.egressPolicy === "internet" ? t("Internet access", "인터넷 허용") : t("Isolated", "격리")}</DetailProp>
        </dl>
      </DetailSection>
      <DetailSection title={t("Port forwarding", "포트 포워딩")}>
        {forwards.length > 0 ? (
          <ul className="console-forwards">
            {forwards.map((pf, index) => (
              <li key={`${pf.protocol}-${pf.hostPort}-${pf.guestPort}-${index}`}>:{pf.guestPort} → :{pf.hostPort}/{pf.protocol}</li>
            ))}
          </ul>
        ) : (
          <p className="detail-note">{t("No port forwarding rules.", "포트 포워딩 규칙이 없습니다.")}</p>
        )}
      </DetailSection>
      <DetailSection title={t("Storage", "스토리지")}>
        <dl className="props">
          <DetailProp label={t("Storage", "스토리지")}>{vm.storageRoot || "default"}</DetailProp>
        </dl>
      </DetailSection>
    </>
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
