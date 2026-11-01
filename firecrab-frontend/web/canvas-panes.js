// Browser/library binding only. Rust owns identity, authorization, leases and WebSockets.
const panes=new Map();
const editors=new Map();
let editorObserver;
function report(key,type,value) {
  document.dispatchEvent(new CustomEvent('firecrab-canvas-terminal',{detail:JSON.stringify({key,type,...value})}));
}
window.addEventListener('message',event=>{
  if(event.data?.type!=='firecrab-window-proof'||!/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/.test(event.data.windowProof||''))return;
  for(const editor of editors.values()) {
    if(event.source===editor.contentWindow&&event.origin===new URL(editor.src).origin&&event.data.client===editor.dataset.client&&event.data.challenge===editor.challenge)editor.windowProof=event.data.windowProof;
  }
});
// A page unload cannot await network writes. Warn until managed surfaces are
// explicitly drained; a forced browser shutdown still cannot restore RAM.
window.addEventListener('beforeunload',event=>{
  if(!editors.size) return;
  event.preventDefault();event.returnValue='';
});
export function canvasPanes(action,payload) {
  if(['has-editor','editor-proof','can-attach-editor','freeze-editor','detach-editor'].includes(action)) {
    const state=JSON.parse(payload), editor=editors.get(state.id);
    if(action==='has-editor') return !!editor;
    if(action==='editor-proof') return editor?.dataset.client===state.client?editor.windowProof||null:null;
    if(action==='can-attach-editor') return !editor||(editor.dataset.client===(state.client||'')&&editor.getAttribute('src')===state.url);
    if(!editor||editor.dataset.client!==state.client) return false;
    if(action==='freeze-editor') {if(!state.value&&editor.windowProof!==state.windowProof)return false;editor.inert=state.value;if(state.value)editor.blur();return true;}
    if(!state.windowProof||editor.windowProof!==state.windowProof)return false;
    editor.remove();editors.delete(state.id);editorObserver?.disconnect();return true;
  }
  if(action==='prepare') {
    for(const pane of panes.values()) pane.container.remove();
    for(const editor of editors.values()) editor.hidden=true;
    editorObserver?.disconnect();return;
  }
  if(action==='attach') {
    const state=JSON.parse(payload);
    for(const [id,pane] of panes) if(!state.keep.includes(id)){pane.abort.abort();pane.session?.dispose();panes.delete(id);}
    for(const surface of state.surfaces) {
      const placeholder=document.getElementById(surface.element);if(!placeholder) continue;
      let pane=panes.get(surface.id);
      if(!pane) {
        pane={container:placeholder,key:surface.key,queue:[],abort:new AbortController()};panes.set(surface.id,pane);
        const current=pane;
        import('./terminal/engine.js').then(async({createWasmTerminal})=>{
          const mirror=document.createElement('pre');mirror.className='terminal-output';mirror.setAttribute('role','log');mirror.setAttribute('aria-label','Terminal output');current.container.appendChild(mirror);
          const session=await createWasmTerminal(current.container,surface.options,current.abort.signal,
            output=>{mirror.textContent=output;},()=>{},
            bytes=>report(current.key,'input',{bytes:Array.from(bytes)}),
            dimensions=>report(current.key,'resize',dimensions));
          current.session=session;
          for(const bytes of current.queue) session.write(bytes);current.queue=[];
          if(current.shared) session.setSharedDimensions(current.shared);
        }).catch(error=>{if(!current.abort.signal.aborted)current.container.textContent=String(error);});
      } else {placeholder.replaceWith(pane.container);pane.session?.fit();}
    }
    // Closed canvas tabs retain their editor client: pending private drafts must
    // finish, and reopening a saved canvas resumes the same editor document.
    if(state.editor) {
      let editor=editors.get(state.editor.id);
      if(!editor) {
        editor=document.createElement('iframe');editor.className='cw-editor-portal';editor.title='Web VS Code';
        editor.setAttribute('sandbox','allow-scripts allow-same-origin allow-forms allow-downloads allow-modals');
        editor.dataset.client=state.editor.client||'';
        editor.onload=()=>{
          if(!editor.dataset.client)return;
          editor.windowProof=null;editor.challenge=crypto.randomUUID();
          editor.contentWindow.postMessage({type:'firecrab-window-proof',client:editor.dataset.client,challenge:editor.challenge},new URL(editor.src).origin);
        };
        editor.setAttribute('allow','clipboard-read; clipboard-write');editor.src=state.editor.url;document.body.appendChild(editor);editors.set(state.editor.id,editor);
      }
      // Never reload a live frame to switch an instance/folder before its flush acknowledgement.
      if(editor.getAttribute('src')&&editor.getAttribute('src')!==state.editor.url) return false;
      if(editor.dataset.client!==(state.editor.client||'')) return false;
      const slot=document.getElementById('cw-editor-slot');
      if(slot) {
        const place=()=>{const rect=slot.getBoundingClientRect();editor.style.left=`${rect.left}px`;editor.style.top=`${rect.top}px`;editor.style.width=`${rect.width}px`;editor.style.height=`${rect.height}px`;};
        editor.hidden=false;place();editorObserver=new ResizeObserver(place);editorObserver.observe(slot);
      }
    }
    return;
  }
  if(typeof payload==='string') payload=JSON.parse(payload);
  for(const pane of panes.values())if(pane.key===payload.key) {
    if(action==='write'){if(pane.session)pane.session.write(payload.bytes);else pane.queue.push(payload.bytes);}
    if(action==='report')pane.session?.reportDimensions();
    if(action==='shared'){pane.shared={rows:payload.rows,cols:payload.cols};pane.session?.setSharedDimensions(pane.shared);}
  }
}
