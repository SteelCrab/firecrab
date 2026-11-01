import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

const status = document.getElementById("terminal-status")!;
const root = document.getElementById("terminal")!;
let korean = false;
try {
  korean = localStorage.getItem("firecrab.locale") === "ko";
} catch {
  /* Locale is optional. */
}
const terminal = new Terminal({
  fontSize: 14,
  fontFamily: "ui-monospace, monospace",
  screenReaderMode: true,
  theme: { background: "#151515", foreground: "#eeeeee" },
});
const fit = new FitAddon();
terminal.loadAddon(fit);
terminal.open(root);
const socket = new WebSocket(
  `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/_fc/terminal/socket`,
);
socket.binaryType = "arraybuffer";
const encoder = new TextEncoder();
let ended = false;
function resize() {
  if (ended) return;
  fit.fit();
  if (socket.readyState === WebSocket.OPEN)
    socket.send(
      JSON.stringify({
        cols: Math.max(2, Math.min(1000, terminal.cols)),
        rows: Math.max(2, Math.min(1000, terminal.rows)),
      }),
    );
}
const observer = new ResizeObserver(resize);
observer.observe(root);
function close() {
  if (ended) return;
  ended = true;
  observer.disconnect();
  socket.close();
  terminal.options.disableStdin = true;
  status.textContent = korean
    ? "연결이 종료되었습니다. 워크스페이스에서 다시 연결해 주세요."
    : "Disconnected. Return to the workspace to reconnect.";
}
terminal.onData((text) => {
  if (socket.readyState !== WebSocket.OPEN || ended) return;
  const bytes = encoder.encode(text);
  for (let offset = 0; offset < bytes.length; offset += 16384)
    socket.send(bytes.subarray(offset, offset + 16384));
});
socket.addEventListener("open", () => {
  status.textContent = "/home/fc-code/project";
  resize();
  terminal.focus();
});
socket.addEventListener("message", (event) => {
  if (event.data instanceof ArrayBuffer)
    terminal.write(new Uint8Array(event.data));
  else close();
});
socket.addEventListener("close", close, { once: true });
socket.addEventListener("error", close, { once: true });
document.getElementById("terminal-close")!.addEventListener("click", close);
window.addEventListener(
  "pagehide",
  () => {
    close();
    terminal.dispose();
  },
  { once: true },
);
