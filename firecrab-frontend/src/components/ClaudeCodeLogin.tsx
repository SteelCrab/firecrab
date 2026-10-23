import { useEffect, useRef, useState } from "react";
import { Copy, SquareTerminal, X } from "lucide-react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { useI18n } from "../i18n";
import { defaultInteractiveTerminalOptions, enableTerminalIme } from "../lib/terminal";
import type { CliLogin } from "../api/pipeAi";

export default function ClaudeCodeLogin({ login, onClose }: { login: CliLogin; onClose: () => void }) {
  const { t } = useI18n();
  const container = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const [ended, setEnded] = useState(false);

  useEffect(() => {
    if (!container.current) return;
    const term = new Terminal(defaultInteractiveTerminalOptions({ scrollback: 0, theme: { background: "#18181b", foreground: "#e4e4e7" } }));
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container.current);
    fit.fit();
    enableTerminalIme(term, container.current);
    terminal.current = term;
    const url = new URL(`/ws/pipe/ai/connections/${encodeURIComponent(login.connection.id)}/claude-code/login`, window.location.href);
    url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(url);
    socket.binaryType = "arraybuffer";
    function resize() {
      fit.fit();
      if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    }
    socket.onopen = () => { socket.send(JSON.stringify({ ticket: login.ticket })); resize(); term.focus(); };
    socket.onmessage = event => {
      if (event.data instanceof ArrayBuffer) term.write(new Uint8Array(event.data));
      else {
        try { if (JSON.parse(event.data).type === "exit") { setEnded(true); onClose(); } }
        catch { /* Only terminal bytes and a fixed completion event are accepted. */ }
      }
    };
    socket.onclose = () => { setEnded(true); onClose(); };
    socket.onerror = () => { setEnded(true); };
    const input = term.onData(data => {
      if (socket.readyState === WebSocket.OPEN) socket.send(new TextEncoder().encode(data));
    });
    const observer = new ResizeObserver(resize);
    observer.observe(container.current);
    return () => {
      observer.disconnect(); input.dispose();
      socket.onclose = null; socket.onmessage = null;
      socket.close(); term.dispose(); terminal.current = null;
    };
  }, [login.connection.id, login.ticket, onClose]);

  return <section className="panel pipe-ai-cli-login" aria-label={t("Claude Code sign-in terminal", "Claude Code 로그인 터미널")}>
    <div className="pipe-ai-section-head"><h3><SquareTerminal aria-hidden="true" />{t("Sign in with the official CLI", "공식 CLI로 로그인한다")}</h3>
      <button type="button" className="icon-btn" aria-label={t("Close login terminal", "로그인 터미널 닫기")} onClick={onClose}><X aria-hidden="true" /></button></div>
    <p className="pipe-ai-hint">{t("Open the URL shown in the terminal in your browser. Paste the code here if prompted. Claude Code manages your credentials.", "터미널에 표시된 URL을 브라우저에서 연다. 코드 입력을 요청하면 이 터미널에 붙여 넣는다. 인증 정보는 Claude Code가 관리한다.")}</p>
    <div className="pipe-ai-terminal" ref={container} />
    <div className="pipe-ai-actions"><button type="button" className="btn sm" onClick={() => { const selection = terminal.current?.getSelection(); if (selection) void navigator.clipboard.writeText(selection); }}><Copy aria-hidden="true" />{t("Copy selection", "선택한 텍스트 복사")}</button>
      <span className="pipe-ai-hint" role="status">{ended ? t("Terminal closed. Checking sign-in…", "터미널을 닫았다. 로그인 상태를 확인한다…") : t("Login only · API host", "로그인 전용 · API 호스트")}</span></div>
  </section>;
}
