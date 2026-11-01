// Browser extension: controller identity never enters a guest extension host.
const vscode = require('vscode');
const versions = new Map();
const drafts = new Map();
const pending = new Map();
const writes = new Map();
const blocked = new Map();
const observations = new Map();
const desired = new Map();
const delays = new Map();
let restoring = false;
let disposed = false;
let closing = false;
let draftEpoch = 0;
const sharedWrites = new Set();
const encode = bytes => {
  let value = '';
  for (let i=0;i<bytes.length;i+=8192) value += String.fromCharCode(...bytes.subarray(i,i+8192));
  return btoa(value);
};
const decode = value => Uint8Array.from(atob(value),c=>c.charCodeAt(0));
const relative = uri => uri.path.replace(/^\//,'');
function error(status,message,code) {
  const failure = status===404 ? vscode.FileSystemError.FileNotFound(message)
    : status===403 ? vscode.FileSystemError.NoPermissions(message)
    : vscode.FileSystemError.Unavailable(message);
  failure.status=status;failure.backendCode=code;return failure;
}
async function api(endpoint,method='GET',body) {
  for(let attempt=0;attempt<4;attempt++) {
    const abort=new AbortController();const timer=setTimeout(()=>abort.abort(),20000);
    try {
      const response=await fetch(`${globalThis.location.origin}/_firecrab/${endpoint}`,{
        method,cache:'no-store',signal:abort.signal,headers:body?{'Content-Type':'application/json'}:{},
        body:body?JSON.stringify(body):undefined,
      });
      const result=await response.json();
      if(!response.ok) {
        if(result.error?.code==='workspace_busy' && attempt<3) {await new Promise(resolve=>setTimeout(resolve,100*(attempt+1)));continue;}
        throw error(response.status,result.error?.message||`HTTP ${response.status}`,result.error?.code);
      }
      return result;
    } catch(failure) {
      // Only stable mutation identities can be retried after an unknown response.
      if(failure.status || attempt===3 || (method!=='GET'&&!body?.operationId)) throw failure;
      await new Promise(resolve=>setTimeout(resolve,100*(attempt+1)));
    } finally {clearTimeout(timer);}
  }
}
const query = name => `?path=${encodeURIComponent(name)}`;
async function load(name,reading=false) {
  let file;
  try {file=await api(`files${query(name)}`);}
  catch(failure) {
    if(failure.status!==404) throw failure;
    let buffer;
    try {buffer=await api(`draft${query(name)}`);}catch(_){throw failure;}
    if(buffer.contentBase64===null) throw failure;
    // An absent shared file can have a private, dirty new-file placeholder.
    file={version:{id:buffer.baseVersion,byteLength:0,createdAt:0},contentBase64:''};
  }
  if(reading) {
    await observeDraft(name);
    observations.set(name,{version:file.version.id,content:file.contentBase64});
    const dirty=vscode.workspace.textDocuments.some(d=>d.uri.scheme==='firecrab'&&relative(d.uri)===name&&d.isDirty);
    if(!versions.has(name)||!dirty) versions.set(name,file.version.id);
  }
  return file;
}
function enqueue(name,work) {
  const next=(pending.get(name)||Promise.resolve()).catch(()=>{}).then(work);
  pending.set(name,next);return next;
}
async function draftHead(name) {
  if (drafts.has(name)) return drafts.get(name);
  try { const value=await api(`draft${query(name)}`);drafts.set(name,value.revision);return value.revision; }
  catch(error) { if(error.status!==404) throw error;drafts.set(name,0);return 0; }
}
// Never adopt another window's unseen dirty draft as the base of a first write.
async function observeDraft(name) {
  if(drafts.has(name)) return;
  try {
    const value=await api(`draft${query(name)}`);
    if(value.contentBase64!==null) {
      drafts.set(name,0);
      blocked.set(name,error(409,'Another window has a private draft. Preserve both copies before saving.'));
    } else drafts.set(name,value.revision);
  } catch(failure) {if(failure.status!==404)throw failure;drafts.set(name,0);}
}
function snapshot(document) {
  const name=relative(document.uri);
  if(document.isClosed) return;
  const content=document.isDirty?encode(new TextEncoder().encode(document.getText())):null;
  const read=observations.get(name);
  if(!document.isDirty&&read&&encode(new TextEncoder().encode(document.getText()))===read.content) versions.set(name,read.version);
  const next={content,baseVersion:versions.get(name)||0}, previous=desired.get(name);
  if(!previous||previous.content!==next.content||previous.baseVersion!==next.baseVersion) draftEpoch++;
  desired.set(name,next);
}
async function persist(name) {
  if(blocked.has(name)) throw blocked.get(name);
  try { await flushDraft(name); } catch(failure) {
    if(failure.status>=400 && failure.status<500 && failure.backendCode!=='workspace_busy') blocked.set(name,failure);
    throw failure;
  }
}
async function flushDraft(name) {
  const previous=writes.get(name);
  if(previous) {
    const result=await api(`draft${query(name)}`,previous.method,previous.body);
    drafts.set(name,result.revision);writes.delete(name);
  }
  const target=desired.get(name);if(!target) return;
  const expectedRevision=await draftHead(name);const operationId=crypto.randomUUID();
  const mutation=target.content!==null
    ? {method:'PUT',body:{operationId,expectedRevision,baseVersion:target.baseVersion,contentBase64:target.content}}
    : expectedRevision ? {method:'DELETE',body:{operationId,expectedRevision}} : null;
  if(!mutation) return;
  writes.set(name,mutation);
  const result=await api(`draft${query(name)}`,mutation.method,mutation.body);
  drafts.set(name,result.revision);writes.delete(name);
}
exports.activate = async context => {
  const changes=new vscode.EventEmitter();
  const status=vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left,5);
  function failed(error) { status.text='$(warning) Draft not saved';status.tooltip=String(error);status.show(); }
  const provider={
    onDidChangeFile:changes.event, watch:()=>new vscode.Disposable(()=>{}),
    async stat(uri) {
      const name=relative(uri);
      if(!name) return {type:vscode.FileType.Directory,ctime:0,mtime:0,size:0};
      try { const file=await load(name);return {type:vscode.FileType.File,ctime:0,mtime:file.version.createdAt*1000,size:file.version.byteLength}; }
      catch(failure) {
        if(![400,404].includes(failure.status)) throw failure;
        await api(`tree${query(name)}`);return {type:vscode.FileType.Directory,ctime:0,mtime:0,size:0};
      }
    },
    async readDirectory(uri) {
      const children=await api(`tree${query(relative(uri))}`);
      return children.map(entry=>[entry.name,entry.directory?vscode.FileType.Directory:vscode.FileType.File]);
    },
    async readFile(uri) { return decode((await load(relative(uri),true)).contentBase64); },
    writeFile(uri,content,options) {
      if(closing) return Promise.reject(vscode.FileSystemError.NoPermissions('This editor is closing. Keep it open to continue editing.'));
      const work=(async()=>{
      const name=relative(uri);
      if(!versions.has(name)) {
        try { await load(name,true);if(!options.overwrite) throw vscode.FileSystemError.FileExists(uri); }
        catch(failure) { if(failure.status!==404) throw failure;versions.set(name,0); }
      }
      await observeDraft(name);
      if(blocked.has(name)) throw blocked.get(name);
      const request={operationId:crypto.randomUUID(),expectedVersion:versions.get(name),contentBase64:encode(content)};
      // Keep the saved base on conflict. VS Code retains the dirty editor document.
      const saved=await api(`files${query(name)}`,'PUT',request);
      versions.set(name,saved.version.id);
      observations.set(name,{version:saved.version.id,content:request.contentBase64});
      if(desired.has(name)) desired.get(name).baseVersion=saved.version.id;
      changes.fire([{type:vscode.FileChangeType.Changed,uri}]);
      })();
      sharedWrites.add(work);
      return work.finally(()=>sharedWrites.delete(work));
    },
    createDirectory:()=>{throw vscode.FileSystemError.NoPermissions('Create folders from a project terminal.');},
    delete:()=>{throw vscode.FileSystemError.NoPermissions('Delete files from a project terminal.');},
    rename:()=>{throw vscode.FileSystemError.NoPermissions('Rename files from a project terminal.');},
  };
  let registration=vscode.workspace.registerFileSystemProvider('firecrab',provider,{isCaseSensitive:true});
  context.subscriptions.push(changes,status,new vscode.Disposable(()=>registration.dispose()));
  async function readOnly(value) {
    registration.dispose();
    registration=vscode.workspace.registerFileSystemProvider('firecrab',provider,{isCaseSensitive:true,isReadonly:value});
    await vscode.commands.executeCommand('setContext','firecrab.closing',value);
  }
  function queue(document,immediate=false) {
    if(restoring||document.uri.scheme!=='firecrab') return;
    const name=relative(document.uri);snapshot(document);clearTimeout(delays.get(name));
    delays.delete(name);
    if(closing) return;
    delays.set(name,setTimeout(()=>{
      delays.delete(name);enqueue(name,()=>persist(name)).then(()=>status.hide(),failed);
    },immediate?0:400));
  }
  context.subscriptions.push(vscode.workspace.onDidChangeTextDocument(event=>queue(event.document)));
  context.subscriptions.push(vscode.workspace.onDidSaveTextDocument(document=>queue(document,true)));
  context.subscriptions.push(vscode.workspace.onDidCloseTextDocument(document=>queue(document,true)));
  context.subscriptions.push(vscode.commands.registerCommand('firecrabWorkspace.recoverDrafts',async()=>{
    const buffers=[...desired].filter(([,value])=>value.content!==null).map(([path,value])=>({path,baseVersion:value.baseVersion,content:new TextDecoder().decode(decode(value.content))}));
    const copy=await vscode.workspace.openTextDocument({language:'json',content:JSON.stringify({buffers},null,2)});
    await vscode.window.showTextDocument(copy,{preview:false});
  }));
  context.subscriptions.push(vscode.commands.registerCommand('firecrabWorkspace.retryDrafts',async()=>{
    for(const name of [...blocked.keys()]) {blocked.delete(name);try {await enqueue(name,()=>persist(name));}catch(failure){failed(failure);}}
  }));
  const clientId=vscode.workspace.getConfiguration('firecrab').inspect('browserClientId')?.defaultValue;
  if(typeof clientId!=='string'||!/^[0-9a-f-]{36}$/.test(clientId)) throw Error('Open this editor from AI Canvas to register its browser client.');
  const windowProof=vscode.workspace.getConfiguration('firecrab').inspect('browserWindowProof')?.defaultValue;
  if(typeof windowProof!=='string'||!/^[0-9a-f-]{36}$/.test(windowProof)) throw Error('The editor window is not registered. Reopen it from AI Canvas.');
  const clientPath=`clients/${clientId}`;
  let command='';
  // VS Code waits for activation before serving this filesystem. Opening our
  // own files inside activate() would deadlock retained-draft restoration.
  async function restoreAndStart() {
  if(disposed) return;
  const session=await api('session');
  const folder=vscode.Uri.parse(`firecrab://${session.id}/${session.workingDirectory||''}`);
  // The workspace URI is stable for this private editor/tab and survives reconnects.
  const retained=await api('drafts');
  restoring=true;
  try {
    for(const entry of retained) {
      const saved=await api(`draft${query(entry.path)}`);drafts.set(entry.path,saved.revision);
      if(saved.contentBase64===null) continue;
      const uri=folder.with({path:`/${entry.path}`});
      const document=await vscode.workspace.openTextDocument(uri);
      versions.set(entry.path,saved.baseVersion);
      const edit=new vscode.WorkspaceEdit();
      edit.replace(uri,new vscode.Range(0,0,document.lineCount,0),new TextDecoder().decode(decode(saved.contentBase64)));
      await vscode.workspace.applyEdit(edit);
      await vscode.window.showTextDocument(document,{preview:false,preserveFocus:true});
    }
  } finally { restoring=false; }
  // Restore open files independently from dirty buffers. A retained close copy
  // never overwrites a private draft subsequently accepted from another window.
  let recent=[];
  try {
    recent=await api('closed-snapshots');
    if(recent.length) {
      const saved=await api(`closed-snapshots/${recent[0].client}/${recent[0].revision}`);
      let conflict=false;
      for(const file of saved.files) if(file.contentBase64!==null) {
        let current;try {current=await api(`draft${query(file.path)}`);}catch(failure){if(failure.status!==404)throw failure;}
        if(!current||current.revision!==file.revision||current.contentBase64!==file.contentBase64) conflict=true;
      }
      for(const name of saved.openFiles) {
        try {await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(folder.with({path:`/${name}`})),{preview:false,preserveFocus:true});}
        catch(failure){failed(failure);}
      }
      if(saved.activeFile) {
        try {await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(folder.with({path:`/${saved.activeFile}`})),{preview:false});}
        catch(failure){failed(failure);}
      }
      if(conflict) vscode.window.showWarningMessage('An earlier closed draft changed in another window. Use Firecrab: Open Closed Editor Recovery Copy to preserve both versions.');
    }
  } catch(failure) {failed(failure);}
  context.subscriptions.push(vscode.commands.registerCommand('firecrabWorkspace.closedDrafts',async()=>{
    const rows=await api('closed-snapshots');
    if(!rows.length) return vscode.window.showInformationMessage('No closed editor recovery copies are available.');
    const chosen=await vscode.window.showQuickPick(rows.map((row,index)=>({label:row.activeFile||`Closed editor ${index+1}`,description:row.createdAt?new Date(row.createdAt*1000).toLocaleString():undefined,row})));
    if(!chosen) return;
    const saved=await api(`closed-snapshots/${chosen.row.client}/${chosen.row.revision}`);
    const buffers=saved.files.filter(file=>file.contentBase64!==null).map(file=>({path:file.path,baseVersion:file.baseVersion,content:new TextDecoder().decode(decode(file.contentBase64))}));
    const copy=await vscode.workspace.openTextDocument({language:'json',content:JSON.stringify({openFiles:saved.openFiles,activeFile:saved.activeFile,buffers},null,2)});
    await vscode.window.showTextDocument(copy,{preview:false});
  }));
  function openFiles() {
    const paths=[];
    for(const group of vscode.window.tabGroups.all) for(const tab of group.tabs) {
      const uri=tab.input?.uri;
      const document=uri&&vscode.workspace.textDocuments.find(d=>d.uri.toString()===uri.toString());
      if(tab.isDirty&&(!uri||uri.scheme!=='firecrab'||!document)) throw Error('Save or export untitled and custom-editor changes before closing this tab.');
      if(uri?.scheme==='firecrab'&&!paths.includes(relative(uri))) paths.push(relative(uri));
    }
    if(vscode.workspace.textDocuments.some(d=>d.isDirty&&d.uri.scheme!=='firecrab')) throw Error('Save or export untitled changes before closing this tab.');
    if(paths.length>32) throw Error('Close some saved files before closing this editor; at most 32 open files can be retained.');
    return paths;
  }
  async function flushClose(state) {
    closing=true;status.text='$(sync~spin) Saving private drafts';status.show();
    for(const handle of delays.values()) clearTimeout(handle);delays.clear();
    await readOnly(true);
    await Promise.allSettled([...sharedWrites]);
    flushAttempts: for(let attempt=0;attempt<4;attempt++) {
      for(const document of vscode.workspace.textDocuments) if(document.uri.scheme==='firecrab') snapshot(document);
      const opened=openFiles(), epoch=draftEpoch;
      const paths=[...new Set([...[...desired].filter(([name,value])=>value.content!==null||(drafts.get(name)||0)>0&&opened.includes(name)).map(([name])=>name),...writes.keys()])];
      if(paths.length>64) throw Error('Too many pending buffers to close safely. Keep the editor open and preserve recovery copies.');
      for(const name of paths) {await observeDraft(name);if(desired.has(name)||writes.has(name)) await enqueue(name,()=>persist(name));}
      if(epoch!==draftEpoch) continue;
      const proofs=[];
      for(const name of paths) {
        let draft;try {draft=await api(`draft${query(name)}`);}catch(failure){if(failure.status!==404)throw failure;draft={revision:0,contentBase64:null};}
        if(epoch!==draftEpoch) continue flushAttempts;
        if(draft.revision!==(drafts.get(name)||0)||draft.contentBase64!==(desired.get(name)?.content??null)) throw Error('A private draft changed in another window. Preserve both copies before closing.');
        const hash=draft.contentBase64===null?null:Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',decode(draft.contentBase64))),b=>b.toString(16).padStart(2,'0')).join('');
        proofs.push({path:name,revision:draft.revision,sha256:hash});
      }
      if(epoch!==draftEpoch) continue;
      const current=await api(clientPath);
      if(current.revision!==state.revision||current.command!=='close'||current.windowProof!==windowProof) return;
      const active=vscode.window.activeTextEditor?.document.uri;
      const activeFile=active?.scheme==='firecrab'&&opened.includes(relative(active))?relative(active):null;
      await api(`${clientPath}/report`,'POST',{action:'flushed',revision:state.revision,windowProof,files:proofs,openFiles:opened,activeFile});
      status.text='$(check) Private drafts saved';status.show();return;
    }
    throw Error('Documents changed while closing. Keep this editor open and retry.');
  }
  let pollBusy=false, handled=null;
  async function tick() {
    if(disposed||pollBusy) return;pollBusy=true;
    try {
      const client=await api(clientPath);
      if(client.windowProof&&client.windowProof!==windowProof) {
        if(closing){await readOnly(false);closing=false;handled=null;for(const document of vscode.workspace.textDocuments)queue(document,true);}
      } else if(client.command==='close') {
        if(handled!==client.revision) {
          handled=client.revision;
          try {await flushClose(client);}
          catch(failure) {
            failed(failure);
            // A lost acknowledgement must not downgrade an already retained snapshot.
            const current=await api(clientPath);
            if(current.revision===client.revision&&current.command==='close'&&current.status!=='ready') {
              await api(`${clientPath}/report`,'POST',{action:'error',revision:client.revision,windowProof,message:String(failure).slice(0,500)});
            }
          }
        }
        return;
      }
      if(client.command==='detach'&&client.windowProof===windowProof) return;
      if(closing) {await readOnly(false);closing=false;handled=null;for(const document of vscode.workspace.textDocuments) queue(document,true);}
      if(client.status!=='active'&&(!client.windowProof||client.windowProof===windowProof)) await api(`${clientPath}/report`,'POST',{action:'active',revision:client.revision,windowProof});
      for(const name of writes.keys()) if(!blocked.has(name)&&!delays.has(name)) {
        delays.set(name,true);enqueue(name,()=>persist(name)).then(()=>status.hide(),failed).finally(()=>delays.delete(name));
      }
      const state=await api('session');
      if(state.openRequest && state.openRequest!==command) {
        command=state.openRequest;
        if(state.file) {
          const uri=folder.with({path:`/${state.file}`});
          await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri),{preview:false});
        }
      }
    } catch(failure) {failed(failure);} finally {pollBusy=false;}
  }
  const timer=setInterval(tick,500);
  await tick();
  context.subscriptions.push(new vscode.Disposable(()=>{disposed=true;clearInterval(timer);for(const handle of delays.values()) clearTimeout(handle);}));
  }
  const startup=setTimeout(async()=>{try {await restoreAndStart();}catch(failure){failed(failure);}},0);
  context.subscriptions.push(new vscode.Disposable(()=>{disposed=true;clearTimeout(startup);}));
};
exports.deactivate=()=>{disposed=true;};
