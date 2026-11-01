import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import {webcrypto,randomUUID,createHash} from 'node:crypto';

const source=fs.readFileSync(new URL('../../firecrab-api/src/project_workspace/editors/editor/extension.js',import.meta.url),'utf8');
const client='55000000-0000-4000-8000-000000000010';
const windowProof='55000000-0000-4000-8000-000000000099';
function uri(name,scheme='firecrab') {return {scheme,path:`/${name}`,with(change){return {...this,...change};},toString(){return `${this.scheme}://editor${this.path}`;}};}
async function harness(options={}) {
  const startup=[];let activated=false;
  const handlers={},commands=new Map(),documents=[],tabs=[],states=[],drafts=new Map(),receipts=new Map(),draftRequests=[],snapshots=[];
  let readonly=false,interval,timerId=0,current={id:client,revision:0,command:'active',status:'opening'},loseDraft=false,loseAck=false;
  let savedText='shared text',savedVersion=1,changeDuringDraft=null,sharedGate=null;
  const vscode={
    FileSystemError:{FileNotFound:m=>new Error(m),NoPermissions:m=>new Error(m),Unavailable:m=>new Error(m),FileExists:m=>new Error(m)},
    FileType:{File:1,Directory:2},FileChangeType:{Changed:1},StatusBarAlignment:{Left:1},
    Disposable:class {constructor(fn){this.dispose=fn;}},EventEmitter:class{event=()=>{};fire(){}dispose(){}},
    Uri:{parse:()=>uri('')},Range:class{},WorkspaceEdit:class{replace(resource,range,text){this.resource=resource;this.text=text;}},
    workspace:{textDocuments:documents,
      getConfiguration:()=>({inspect:name=>({defaultValue:name==='browserClientId'?client:windowProof})}),
      registerFileSystemProvider:(scheme,provider,options)=>{vscode.provider=provider;readonly=!!options.isReadonly;return {dispose(){}};},
      onDidChangeTextDocument:fn=>(handlers.change=fn,{dispose(){}}),onDidSaveTextDocument:fn=>(handlers.save=fn,{dispose(){}}),onDidCloseTextDocument:fn=>(handlers.close=fn,{dispose(){}}),
      async openTextDocument(input){
        assert.equal(activated,true,'The virtual filesystem cannot open documents until activate has returned');
        if(input.content!==undefined){const doc={uri:uri(`recovery-${documents.length}`,'untitled'),version:1,isDirty:true,isClosed:false,lineCount:1,getText:()=>input.content};documents.push(doc);tabs.push({input:{uri:doc.uri},isDirty:true});return doc;}
        let doc=documents.find(d=>d.uri.toString()===input.toString());if(doc)return doc;
        const bytes=await vscode.provider.readFile(input);let text=new TextDecoder().decode(bytes);
        doc={uri:input,version:1,isDirty:false,isClosed:false,lineCount:1,getText:()=>text,set(value){text=value;this.version++;this.isDirty=true;tabs.find(t=>t.input.uri.toString()===this.uri.toString()).isDirty=true;handlers.change?.({document:this});}};
        documents.push(doc);tabs.push({input:{uri:input},isDirty:false});return doc;
      },
      async applyEdit(edit){const doc=await this.openTextDocument(edit.resource);doc.set(edit.text);return true;},
    },
    window:{tabGroups:{all:[{tabs}]},activeTextEditor:null,
      createStatusBarItem:()=>({show(){},hide(){},dispose(){}}),
      async showTextDocument(doc){this.activeTextEditor={document:doc};return {};},
      showWarningMessage:message=>states.push({warning:message}),showInformationMessage:message=>states.push({info:message}),
      showQuickPick:async rows=>rows[0],
    },
    commands:{registerCommand:(name,fn)=>(commands.set(name,fn),{dispose(){}}),executeCommand:async()=>{}},
  };
  if(options.currentDraft) drafts.set('note.txt',options.currentDraft);
  const oldSnapshots=options.snapshots??[];
  async function fetch(url,options={}) {
    const parsed=new URL(url),endpoint=parsed.pathname.replace('/_firecrab/',''),name=parsed.searchParams.get('path');
    const method=options.method||'GET',body=options.body?JSON.parse(options.body):undefined;
    let value,status=200;
    if(endpoint==='session') value={id:'editor',workingDirectory:'',openRequest:'initial',file:'note.txt'};
    else if(endpoint==='drafts') value=[...drafts].filter(([,d])=>d.contentBase64!==null).map(([path])=>({path}));
    else if(endpoint==='closed-snapshots') value=oldSnapshots.map((s,i)=>({client:s.client,revision:s.revision,activeFile:s.activeFile,createdAt:i+1}));
    else if(endpoint.startsWith('closed-snapshots/')) value=oldSnapshots.find(s=>endpoint===`closed-snapshots/${s.client}/${s.revision}`);
    else if(endpoint==='files'&&method==='GET') value={version:{id:savedVersion,createdAt:0,byteLength:savedText.length},contentBase64:Buffer.from(savedText).toString('base64')};
    else if(endpoint==='files'&&method==='PUT') {
      if(sharedGate) await sharedGate;
      savedText=Buffer.from(body.contentBase64,'base64').toString();savedVersion++;
      value={version:{id:savedVersion}};
    } else if(endpoint==='draft') {
      if(method==='GET') {value=drafts.get(name);if(!value){status=404;value={error:{message:'Missing draft'}};}}
      else {
        draftRequests.push(structuredClone(body));
        if(receipts.has(body.operationId)) value={revision:receipts.get(body.operationId)};
        else if((drafts.get(name)?.revision??0)!==body.expectedRevision){status=409;value={error:{message:'Private draft changed'}};}
        else {
          const revision=body.expectedRevision+1;
          drafts.set(name,{path:name,revision,baseVersion:body.baseVersion??0,contentBase64:method==='DELETE'?null:body.contentBase64});receipts.set(body.operationId,revision);value={revision};
          if(changeDuringDraft){const fn=changeDuringDraft;changeDuringDraft=null;fn();}
          if(loseDraft){loseDraft=false;status=503;value={error:{message:'Acknowledgement unavailable'}};}
        }
      }
    } else if(endpoint===`clients/${client}`) value=structuredClone(current);
    else if(endpoint===`clients/${client}/report`){
      if(body.revision!==current.revision) {status=409;value={error:{message:'Close command changed'}};}
      else if(body.action==='active'){current.status='active';value=structuredClone(current);}
      else if(body.action==='error'){current.status='error';current.error=body.message;value=structuredClone(current);}
      else if(body.action==='flushed'){
        for(const file of body.files){const draft=drafts.get(file.path);assert.equal(file.revision,draft?.revision??0);assert.equal(file.sha256,draft?.contentBase64==null?null:createHash('sha256').update(Buffer.from(draft.contentBase64,'base64')).digest('hex'));}
        snapshots.push({report:structuredClone(body),drafts:structuredClone([...drafts])});current.status='ready';value=structuredClone(current);
        if(loseAck){loseAck=false;throw Error('Lost close acknowledgement');}
      }
    } else throw Error(`Unexpected endpoint ${method} ${endpoint}`);
    return {ok:status===200,status,json:async()=>structuredClone(value)};
  }
  const sandbox={require:name=>{assert.equal(name,'vscode');return vscode;},exports:{},fetch,crypto:webcrypto,AbortController,TextEncoder,TextDecoder,Uint8Array,Map,Set,Promise,Date,URL,
    btoa:s=>Buffer.from(s,'binary').toString('base64'),atob:s=>Buffer.from(s,'base64').toString('binary'),
    setTimeout:(fn,ms)=>{const id=++timerId;if(ms===0)startup.push(fn);else if(ms<400)queueMicrotask(fn);return id;},clearTimeout:()=>{},setInterval:fn=>(interval=fn,1),clearInterval:()=>{},location:{origin:'http://fixture.editor.localhost'}};
  vm.runInNewContext(source,sandbox,{filename:'extension.js'});
  const context={subscriptions:[]};await sandbox.exports.activate(context);activated=true;for(const work of startup)await work();
  return {vscode,documents,drafts,draftRequests,snapshots,states,commands,
    get readonly(){return readonly;},get current(){return current;},
    tick:()=>interval(),setControl(command,revision){current={...current,command,revision,windowProof,status:command==='close'?'flushing':'resuming',error:null};},
    loseDraftAck(){loseDraft=true;},loseCloseAck(){loseAck=true;},changeDuringDraft(fn){changeDuringDraft=fn;},holdShared(gate){sharedGate=gate;},dispose(){for(const s of context.subscriptions)s.dispose?.();},
  };
}

test('close flushes a dirty document before debounce and retains its exact open-file snapshot',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('private before debounce');
  h.setControl('close',1);await h.tick();
  assert.equal(h.current.status,'ready');assert.equal(h.readonly,true);assert.equal(h.snapshots.length,1);
  assert.deepEqual(h.snapshots[0].report.openFiles,['note.txt']);assert.equal(h.snapshots[0].report.activeFile,'note.txt');
  assert.equal(Buffer.from(h.drafts.get('note.txt').contentBase64,'base64').toString(),'private before debounce');
});

test('a lost draft acknowledgement retries its original operation and never closes on an unconfirmed save',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('retain after unknown response');h.loseDraftAck();
  h.setControl('close',1);await h.tick();assert.equal(h.current.status,'error');assert.equal(h.snapshots.length,0);
  h.setControl('close',2);await h.tick();assert.equal(h.current.status,'ready');
  assert.deepEqual(h.draftRequests[0],h.draftRequests[1]);assert.equal(h.snapshots.length,1);
});

test('a lost snapshot acknowledgement is reconciled without downgrading an accepted snapshot',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('snapshot accepted once');h.loseCloseAck();
  h.setControl('close',1);await h.tick();assert.equal(h.current.status,'ready');assert.equal(h.snapshots.length,1);
  await h.tick();assert.equal(h.snapshots.length,1);
});

test('document changes during persistence must be flushed before close is acknowledged',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('first text');h.changeDuringDraft(()=>h.documents[0].set('later text'));
  h.setControl('close',1);await h.tick();assert.equal(h.current.status,'ready');
  assert.equal(Buffer.from(h.snapshots[0].drafts[0][1].contentBase64,'base64').toString(),'later text');
});

test('unsupported dirty documents block close and cancel restores editing without removing buffers',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('project buffer');
  await h.vscode.workspace.openTextDocument({content:'unbound private text'});
  h.setControl('close',1);await h.tick();assert.equal(h.current.status,'error');assert.equal(h.snapshots.length,0);assert.equal(h.readonly,true);
  h.setControl('cancel',2);await h.tick();assert.equal(h.current.status,'active');assert.equal(h.readonly,false);
  assert.equal(h.documents[0].getText(),'project buffer');assert.equal(h.documents[1].getText(),'unbound private text');
});

test('close waits for an in-flight shared-file save before retaining private state',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('saving now');
  let release;h.holdShared(new Promise(resolve=>release=resolve));
  const save=h.vscode.provider.writeFile(h.documents[0].uri,new TextEncoder().encode('saving now'),{overwrite:true});
  await new Promise(resolve=>setImmediate(resolve));
  h.setControl('close',1);const closing=h.tick();await new Promise(resolve=>setImmediate(resolve));assert.equal(h.snapshots.length,0);
  release();await save;await closing;assert.equal(h.current.status,'ready');assert.equal(h.drafts.get('note.txt').baseVersion,2);
});

test('closed recovery copies do not overwrite a later accepted draft from another window',async t=>{
  const current={revision:2,baseVersion:1,contentBase64:Buffer.from('newer private text').toString('base64')};
  const prior={client:randomUUID(),revision:1,activeFile:'note.txt',openFiles:['note.txt'],files:[{path:'note.txt',revision:1,baseVersion:1,contentBase64:Buffer.from('older closed text').toString('base64')}]};
  const h=await harness({currentDraft:current,snapshots:[prior]});t.after(()=>h.dispose());
  assert.equal(h.documents[0].getText(),'newer private text');assert.equal(h.states.some(s=>s.warning),true);
  await h.commands.get('firecrabWorkspace.closedDrafts')();
  assert.equal(h.documents.at(-1).getText().includes('older closed text'),true);assert.deepEqual(h.drafts.get('note.txt'),current);
});


test('a second workbench opened from the same URL cannot acknowledge another window close',async t=>{
  const h=await harness();t.after(()=>h.dispose());h.documents[0].set('this window remains active');
  h.setControl('close',1);h.current.windowProof=randomUUID();await h.tick();
  assert.equal(h.readonly,false);assert.equal(h.snapshots.length,0);assert.equal(h.current.status,'flushing');
  assert.equal(h.documents[0].getText(),'this window remains active');
});

test('a clean window closes without deleting another window private draft',async t=>{
  const h=await harness();t.after(()=>h.dispose());
  h.drafts.set('note.txt',{revision:1,baseVersion:1,contentBase64:Buffer.from('another window draft').toString('base64')});
  h.setControl('close',1);await h.tick();assert.equal(h.current.status,'ready');
  assert.equal(h.snapshots[0].report.files.length,0);assert.equal(h.snapshots[0].report.openFiles[0],'note.txt');
  assert.equal(h.draftRequests.length,0);assert.equal(Buffer.from(h.drafts.get('note.txt').contentBase64,'base64').toString(),'another window draft');
});
