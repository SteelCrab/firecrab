import "./styles/console.css";
import "./styles/ssh.css";
import { createWasmTerminal } from "./terminal/engine.js";

// The library owns VT parsing and canvas rendering. Rust owns the session.
export async function mountTerminal(options, signal) {
  const container = document.getElementById("terminal");
  const { id, ...settings } = options;
  const mirror = document.createElement("pre");
  mirror.className = "terminal-output";
  mirror.setAttribute("role", "log");
  mirror.setAttribute(
    "aria-label",
    document.documentElement.lang === "ko" ? "터미널 출력" : "Terminal output",
  );
  container.appendChild(mirror);
  const report = (error) =>
    document.dispatchEvent(
      new CustomEvent("firecrab-terminal-error", {
        detail: JSON.stringify({ id, message: error.message ?? String(error) }),
      }),
    );
  let session;
  try {
    session = await createWasmTerminal(
      container,
      settings,
      signal,
      (text) => {
        mirror.textContent = text;
      },
      report,
      (bytes) =>
        document.dispatchEvent(
          new CustomEvent("firecrab-terminal-input", { detail: bytes }),
        ),
      (size) => document.dispatchEvent(new CustomEvent("firecrab-terminal-dimensions", { detail: JSON.stringify({ id, ...size }) })),
    );
  } catch (error) {
    mirror.remove();
    throw error;
  }
  const term = session.terminal;
  const mount = container.querySelector(".terminal-emulator");
  container.style.backgroundColor = settings.theme.background;
  container.closest(".console-surface").style.backgroundColor =
    settings.theme.background;
  return {
    shared: (size) => session.setSharedDimensions(JSON.parse(size)),
    reportDimensions: session.reportDimensions,
    write: (data) =>
      session.write(typeof data === "string" ? data : new Uint8Array(data)),
    options: (next) => {
      term.options.fontSize = next.fontSize;
      mount.style.fontSize = `${next.fontSize}px`;
      term.renderer?.remeasureFont();
      session.fit();
    },
    fit: session.fit,
    focus: () => term.focus(),
    snapshot: session.readText,
    dispose: () => {
      session.dispose();
      mirror.remove();
    },
  };
}
