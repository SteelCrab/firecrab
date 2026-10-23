import '../../firecrab-frontend/src/styles/tokens.css';
import '../../firecrab-frontend/src/styles/base.css';
import '../../firecrab-frontend/src/styles/frame.css';
import '../../firecrab-frontend/src/styles/vm.css';
import './wasm.css';
import init, { terminal_key_allowed } from '../pkg/firecrab_frontend_wasm.js';
import wasmUrl from '../pkg/firecrab_frontend_wasm_bg.wasm?url';

// Thin browser/library bindings; application behavior lives in Rust.
let terminal;
let generation = 0;
window.firecrabTerminalKey = terminal_key_allowed;
window.firecrabTerminal = (action, payload) => {
  if (action === 'dispose') { ++generation; terminal?.dispose(); terminal = undefined; return; }
  if (action === 'mount') {
    const current = ++generation;
    terminal?.dispose(); terminal = undefined;
    return import('./terminal.js').then(({ mountTerminal }) => {
      if (current === generation) terminal = mountTerminal(JSON.parse(payload));
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
