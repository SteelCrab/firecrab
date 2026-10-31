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
function snapshot(document) {
  const name=relative(document.uri);
  if(document.isClosed) return;
  const content=document.isDirty?encode(new TextEncoder().encode(document.getText())):null;
  const read=observations.get(name);
  if(!document.isDirty&&read&&encode(new TextEncoder().encode(document.getText()))===read.content) versions.set(name,read.version);
  desired.set(name,{content,baseVersion:versions.get(name)||0});
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
    async writeFile(uri,content,options) {
      const name=relative(uri);
      if(!versions.has(name)) {
        try { await load(name,true);if(!options.overwrite) throw vscode.FileSystemError.FileExists(uri); }
        catch(failure) { if(failure.status!==404) throw failure;versions.set(name,0); }
      }
      const request={operationId:crypto.randomUUID(),expectedVersion:versions.get(name),contentBase64:encode(content)};
      // Keep the saved base on conflict. VS Code retains the dirty editor document.
      const saved=await api(`files${query(name)}`,'PUT',request);
      versions.set(name,saved.version.id);
      observations.set(name,{version:saved.version.id,content:request.contentBase64});
      if(desired.has(name)) desired.get(name).baseVersion=saved.version.id;
      changes.fire([{type:vscode.FileChangeType.Changed,uri}]);
    },
    createDirectory:()=>{throw vscode.FileSystemError.NoPermissions('Create folders from a project terminal.');},
    delete:()=>{throw vscode.FileSystemError.NoPermissions('Delete files from a project terminal.');},
    rename:()=>{throw vscode.FileSystemError.NoPermissions('Rename files from a project terminal.');},
  };
  context.subscriptions.push(changes,status,vscode.workspace.registerFileSystemProvider('firecrab',provider,{isCaseSensitive:true}));
  function queue(document,immediate=false) {
    if(restoring||document.uri.scheme!=='firecrab') return;
    const name=relative(document.uri);snapshot(document);clearTimeout(delays.get(name));
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
  let command='';
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
  const timer=setInterval(async()=>{
    if(disposed) return;
    try {
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
    } catch(failure) { failed(failure); }
  },1000);
  context.subscriptions.push(new vscode.Disposable(()=>{disposed=true;clearInterval(timer);for(const handle of delays.values()) clearTimeout(handle);}));
};
exports.deactivate=()=>{disposed=true;};
