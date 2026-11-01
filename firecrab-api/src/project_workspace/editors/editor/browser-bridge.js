// Trusted workbench script. Only the actual iframe parent can receive this document's proof.
(()=>{
  const meta=document.getElementById('vscode-workbench-web-configuration');
  const defaults=JSON.parse(meta.getAttribute('data-settings')).configurationDefaults;
  const client=defaults['firecrab.browserClientId'],windowProof=defaults['firecrab.browserWindowProof'],origin=defaults['firecrab.parentOrigin'];
  window.addEventListener('message',event=>{
    if(event.source!==window.parent||event.origin!==origin||event.data?.type!=='firecrab-window-proof'||event.data.client!==client||typeof event.data.challenge!=='string')return;
    window.parent.postMessage({type:'firecrab-window-proof',client,windowProof,challenge:event.data.challenge},origin);
  });
})();
