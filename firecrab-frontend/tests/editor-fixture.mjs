// Stateful browser-controller fixture. This has no VM, provider, or real project files.
import {randomUUID,createHash} from 'node:crypto';
import {installCanvasHistoryFixture} from './canvas-fixture.mjs';
const hash=value=>createHash('sha256').update(value).digest('hex');
export function installCanvasEditorFixture(fixture) {
  const canvas=installCanvasHistoryFixture(fixture),base=canvas.base;
  const editors=new Map(),clients=new Map(),drafts=new Map(),receipts=new Map(),snapshots=new Map(),controls=[],reports=[];
  let savedWrites=0,draftWrites=0,stopRequests=0,denyDrafts=false,loseDraft=false,loseControl=false;
  let saved={version:{id:1,path:'note.txt',sha256:hash('shared fixture text\n'),byteLength:20,createdAt:1},contentBase64:Buffer.from('shared fixture text\n').toString('base64')};
  const old=fixture.matchHandler;
  fixture.handlers.set('GET /api/ai-sandbox/projects',({send})=>send(200,[{id:canvas.project,spec:{name:'Browser close QA · test data'}}]));
  fixture.handlers.set(`GET ${base}/files/tree`,({url,send})=>send(url.searchParams.get('path')?404:200,url.searchParams.get('path')?{error:{message:'Missing directory'}}:[{name:'note.txt',directory:false}]));
  fixture.handlers.set(`GET ${base}/files`,({url,send})=>send(url.searchParams.get('path')==='note.txt'?200:404,url.searchParams.get('path')==='note.txt'?saved:{error:{message:'Missing file'}}));
  fixture.handlers.set(`PUT ${base}/files`,({body,send})=>{
    if(body.expectedVersion!==saved.version.id)return send(409,{error:{message:'Shared file changed'}});
    savedWrites++;saved={version:{...saved.version,id:saved.version.id+1,sha256:hash(Buffer.from(body.contentBase64,'base64'))},contentBase64:body.contentBase64};send(200,saved);
  });
  const failed=(send,message,status=409)=>send(status,{error:{message}});
  fixture.matchHandler=({method,path})=>{
    if(path.startsWith(`${base}/views/`)&&/\/(draft|drafts)$/.test(path)) return ({url,body,send})=>{
      const parts=path.split('/'),view=parts.at(-2),name=url.searchParams.get('path'),key=`${view}:${name}`;
      if(parts.at(-1)==='drafts')return send(200,[...drafts].filter(([key,row])=>key.startsWith(view+':')&&row.contentBase64!==null).map(([,row])=>({path:row.path,revision:row.revision,baseVersion:row.baseVersion,byteLength:Buffer.from(row.contentBase64,'base64').length})));
      if(method==='GET')return drafts.has(key)?send(200,drafts.get(key)):failed(send,'Missing private draft',404);
      if(denyDrafts)return failed(send,'Private draft storage temporarily unavailable',503);
      if(receipts.has(body.operationId))return send(200,{revision:receipts.get(body.operationId)});
      if((drafts.get(key)?.revision??0)!==body.expectedRevision)return failed(send,'Private draft changed; preserve both copies');
      const revision=body.expectedRevision+1;draftWrites++;drafts.set(key,{path:name,revision,baseVersion:body.baseVersion??0,contentBase64:method==='DELETE'?null:body.contentBase64});receipts.set(body.operationId,revision);
      if(loseDraft){loseDraft=false;return failed(send,'Accepted draft acknowledgement unavailable',503);}return send(200,{revision});
    };
    if(path.startsWith(`${base}/editors/`)) return ({url,body,send})=>{
      const parts=path.slice(`${base}/editors/`.length).split('/'),view=parts[0];let e=editors.get(view);
      if(parts.length===1){
        if(method==='GET')return e?send(200,e):failed(send,'Missing editor',404);
        if(!e){e={id:randomUUID(),viewId:view,instance:hash(randomUUID()),host:`f${view.replaceAll('-','')}.editor.localhost`,status:'ready',browserProtocol:1,workingDirectory:body.workingDirectory,file:body.file,openRequest:body.requestId};editors.set(view,e);}
        else {e.file=body.file;e.openRequest=body.requestId;}return send(200,e);
      }
      if(parts[1]==='stop'){stopRequests++;return failed(send,'Browser close must not terminate a retained native process');}
      if(parts[1]==='closed-snapshots'){
        if(parts.length===2)return send(200,[...snapshots.values()].filter(s=>s.view===view).toReversed().map(s=>({client:s.client,revision:s.revision,createdAt:s.createdAt,activeFile:s.activeFile,bytes:JSON.stringify(s).length})));
        const s=snapshots.get(`${parts[2]}:${parts[3]}`);return s&&s.view===view?send(200,s):failed(send,'Missing close snapshot',404);
      }
      if(parts[1]!=='clients'||!e)return failed(send,'Missing editor',404);
      const id=parts[2];let c=clients.get(id);
      if(parts.length===3&&method==='PUT'){
        if(c)return c.view===view&&c.instance===body.instance?send(200,c):failed(send,'Client identity changed');
        c={id,editor:e.id,view,instance:e.instance,revision:0,command:'active',status:'opening',operation:null,error:null,activations:[],windowProof:null};clients.set(id,c);return send(200,c);
      }
      if(!c||c.view!==view)return failed(send,'Missing browser client',404);
      if(parts.length===3)return send(200,c);
      if(parts[3]==='control'){
        controls.push(structuredClone(body));
        if(!c.activations.includes(body.windowProof))return failed(send,'Unregistered browser window proof');
        if(c.operation===body.operationId)return send(200,c);
        if(c.revision!==body.expectedRevision)return failed(send,'Browser close revision changed');
        if(body.action==='detach'&&(c.status!=='ready'||c.windowProof!==body.windowProof))return failed(send,'Private draft snapshot is not confirmed');
        c.windowProof=body.windowProof;c.revision++;c.operation=body.operationId;c.command=body.action;c.status=({close:'flushing',cancel:'resuming',detach:'detached'})[body.action];c.error=null;
        if(loseControl){loseControl=false;return failed(send,'Accepted close command acknowledgement unavailable',503);}return send(200,c);
      }
      reports.push(structuredClone(body));
      if(c.revision!==body.revision||!c.activations.includes(body.windowProof)||c.windowProof&&c.windowProof!==body.windowProof)return failed(send,'Browser command or window changed');
      if(body.action==='active'){c.status='active';return send(200,c);}
      if(body.action==='error'){if(c.status!=='ready'){c.status='error';c.error=body.message;}return send(200,c);}
      if(body.action!=='flushed')return failed(send,'Unknown report');
      const files=[];
      for(const f of body.files){const d=drafts.get(`${view}:${f.path}`)??{path:f.path,revision:0,baseVersion:0,contentBase64:null};
        if(d.revision!==f.revision||(d.contentBase64===null?null:hash(Buffer.from(d.contentBase64,'base64')))!==f.sha256)return failed(send,'Close snapshot bytes changed');files.push(structuredClone(d));}
      const key=`${id}:${c.revision}`;
      if(!snapshots.has(key))snapshots.set(key,{client:id,view,revision:c.revision,windowProof:body.windowProof,files,openFiles:body.openFiles,activeFile:body.activeFile,createdAt:Math.floor(Date.now()/1000)});
      c.status='ready';return send(200,c);
    };
    return old?.({method,path});
  };
  return {...canvas,editors,clients,drafts,snapshots,controls,reports,
    get savedWrites(){return savedWrites;},get savedVersion(){return saved.version.id;},
    get draftWrites(){return draftWrites;},get stopRequests(){return stopRequests;},
    denyDraftWrites(value){denyDrafts=value;},loseNextDraftAck(){loseDraft=true;},loseNextControlAck(){loseControl=true;},
    activate(id,proof){const c=clients.get(id);if(!c)throw Error('Missing browser fixture client');c.activations.push(proof);},
    acknowledge(id){const c=clients.get(id);if(!c)throw Error('Missing fixture client');snapshots.set(`${id}:${c.revision}`,{client:id,view:c.view,revision:c.revision,files:[],openFiles:[],activeFile:null,createdAt:0});c.status='ready';},
    reportActive(id){clients.get(id).status='active';},
    hostEditor(host){return [...editors.values()].find(e=>e.host===host);},
  };
}
