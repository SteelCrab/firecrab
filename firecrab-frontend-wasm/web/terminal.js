import '../../firecrab-frontend/src/styles/console.css';
import '../../firecrab-frontend/src/styles/ssh.css';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

// xterm adapter only. Rust owns connection, retries, preferences and exports.
export function mountTerminal(options) {
  const container = document.getElementById('terminal');
  const { id, ...settings } = options;
  const term = new Terminal(settings);
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(container);
  term.attachCustomKeyEventHandler(window.firecrabTerminalKey);
  const textarea = container.querySelector('textarea');
  if (textarea) {
    textarea.lang = document.documentElement.lang;
    textarea.inputMode = 'text';
    textarea.autocomplete = 'off';
    textarea.autocapitalize = 'off';
    textarea.spellcheck = false;
  }
  const resize = () => requestAnimationFrame(() => {
    if (!container.isConnected || container.clientWidth < 16 || container.clientHeight < 16) return;
    const dimensions = fit.proposeDimensions();
    if (dimensions?.cols >= 2 && dimensions.rows >= 2) fit.fit();
  });
  const observer = new ResizeObserver(resize);
  observer.observe(container);
  const input = term.onData(data => document.dispatchEvent(new CustomEvent('firecrab-terminal-input', { detail: data })));
  resize();
  document.dispatchEvent(new CustomEvent('firecrab-terminal-ready', { detail: id }));
  return {
    write: data => term.write(typeof data === 'string' ? data : new Uint8Array(data)),
    options: next => { term.options.fontSize = next.fontSize; term.options.theme = next.theme; resize(); },
    fit: resize,
    focus: () => term.focus(),
    clear: () => term.reset(),
    snapshot: () => new Promise(resolve => term.write('', () => {
      const buffer = term.buffer.active;
      const lines = Array.from({ length: buffer.length }, (_, index) => buffer.getLine(index)?.translateToString(true) ?? '');
      while (lines.at(-1) === '') lines.pop();
      resolve(lines.join('\n'));
    })),
    dispose: () => { observer.disconnect(); input.dispose(); term.dispose(); },
  };
}
