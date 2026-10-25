import './styles/tokens.css';
import './styles/base.css';
import './styles/frame.css';
import './styles/vm.css';
import './styles/detail.css';
import './wasm.css';
import './styles/pipe-ai.css';
import './styles/ai-sandbox.css';
import './styles/queues.css';
import init, { terminal_key_allowed } from '../pkg/firecrab_frontend.js';
import wasmUrl from '../pkg/firecrab_frontend_bg.wasm?url';

// Thin browser/library bindings; application behavior lives in Rust.
let terminal;
let terminalController;
let generation = 0;
window.firecrabTerminalKey = terminal_key_allowed;
window.firecrabTerminal = (action, payload) => {
  if (action === 'dispose') { ++generation; terminalController?.abort(); terminal?.dispose(); terminal = undefined; return; }
  if (action === 'mount') {
    const current = ++generation;
    terminalController?.abort(); terminal?.dispose(); terminal = undefined;
    const controller = new AbortController(); terminalController = controller;
    return import('./terminal.js').then(async ({ mountTerminal }) => {
      if (current !== generation) return;
      const options = JSON.parse(payload);
      const mounted = await mountTerminal(options, controller.signal);
      if (current !== generation) { mounted.dispose(); return; }
      terminal = mounted;
      document.dispatchEvent(new CustomEvent('firecrab-terminal-ready', { detail: options.id }));
    }).catch(error => {
      if (controller.signal.aborted || current !== generation) return;
      document.dispatchEvent(new CustomEvent('firecrab-terminal-error', {
        detail: JSON.stringify({ id: JSON.parse(payload).id, message: error.message ?? String(error) }),
      }));
    });
  }
  if (action === 'options') return terminal?.options(JSON.parse(payload));
  return terminal?.[action]?.(payload);
};
try { const wasm = await init({ module_or_path: wasmUrl }); window.firecrabWasmMemory = wasm.memory; }
catch (error) {
  const alert = document.createElement('p');
  alert.setAttribute('role', 'alert');
  alert.textContent = `Unable to start firecrab: ${error.message}`;
  document.getElementById('root').replaceChildren(alert);
  console.error(error);
}
