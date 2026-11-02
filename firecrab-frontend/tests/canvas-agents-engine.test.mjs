import {test} from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {FolderAgents} from '../src/canvas/agents.ts';
import {agentDocument,agentLabel,selectionKey} from '../src/canvas/agent-types.ts';
import {newTab} from '../src/canvas/types.ts';
import {WorkspaceError} from '../src/canvas/request.ts';
function fixture(options={}) {
 const project=randomUUID(),connectionId=randomUUID(),tab=newTab(undefined,project),id=randomUUID();
 tab.loaded=true;tab.workspace={status:'ready'};
 const node={id,kind:'agent',x:0,y:0,width:540,height:620,content:JSON.stringify({schema:1,project,directory:'api/auth',draft:'Update saved files',selection:{connectionId,model:'fixture-model'}})};tab.layout.nodes.push(node);
 const calls=[],stored=[],records=new Map();let offline=false,saveGate,owned=true;
 const api=async(method,path,body)=>{
  calls.push({method,path,body:structuredClone(body)});
  if(offline)throw new WorkspaceError('offline');
  if(path==='/api/ai-sandbox/connections')return [{id:connectionId,name:'Fixture',provider:'openai',authentication:'chatgpt'}];
  if(path.includes('/models'))return {models:[{id:options.otherModel?'different-model':'fixture-model',name:'Fixture model',verified:true,toolsVerified:!options.unverified}]};
  if(path.includes('/tasks?'))return [...records.values()];
  if(path.includes('/instructions/snapshots/'))return {id:path.split('/').at(-1),files:[{path:'AGENTS.md',version:7}]};
  if(method==='GET'&&path.startsWith('/api/ai-sandbox/tasks/')){
   const record=records.get(path.split('/').at(-1));if(!record)throw new WorkspaceError('not found',404,'not_found');return structuredClone(record);
  }
  if(method==='PUT'&&path.includes('/tasks/')){
   const record={id:path.split('/').at(-1),projectId:project,request:structuredClone(body),selection:body.selection,state:'result',result:'Saved by fixture',createdAt:1,folderMutations:[]};records.set(record.id,record);
   if(options.loseAdmission){offline=true;throw new WorkspaceError('response lost');}return structuredClone(record);
  }
  throw new Error('Unexpected fixture request '+method+' '+path);
 };
 const host={edit(t){t.changed++;},changed(){},owned:()=>owned,persist:async()=>{if(options.saveFailure)throw new Error('private save failed');stored.push(node.content);if(saveGate)await saveGate;tab.saved=tab.changed;}};
 const manager=new FolderAgents(host,api);
 return {project,node,tab,calls,stored,records,host,api,manager,get offline(){return offline;},set offline(v){offline=v;},set gate(v){saveGate=v;},set owned(v){owned=v;},async ready(){await manager.models(tab,node);await manager.refresh(tab,node);calls.length=0;}};
}
test('folder dispatch waits for the exact private request checkpoint before capturing saved rules',async()=>{
 const f=fixture();await f.ready();let release;f.gate=new Promise(r=>release=r);
 const submitting=f.manager.submit(f.tab,f.node);await new Promise(r=>setImmediate(r));
 assert.equal(f.calls.length,0);const checkpoint=JSON.parse(f.stored[0]);assert.ok(checkpoint.pending);assert.equal(checkpoint.pending.request.coordination.assignedAgent,agentLabel(f.node));
 release();await submitting;
 const writes=f.calls.filter(c=>c.method==='PUT');assert.equal(writes.length,2);assert.deepEqual(writes[0].body,{directory:'api/auth'});
 assert.equal(writes[1].body.instructionSnapshotId,checkpoint.pending.snapshot);assert.ok(writes[1].path.endsWith(checkpoint.pending.id));
 assert.equal(agentDocument(f.node,f.project).pending,undefined);assert.equal(f.records.size,1);
});
test('a failed private save cannot capture rules or admit a model task',async()=>{
 const f=fixture({saveFailure:true});await f.ready();await f.manager.submit(f.tab,f.node);
 assert.equal(f.calls.filter(c=>c.method==='PUT').length,0);assert.equal(f.records.size,0);assert.match(f.manager.get(f.tab,f.node).error,/private save failed/);assert.ok(agentDocument(f.node,f.project).pending);
});
test('lost admission responses recover by the original task ID after reload without another dispatch',async()=>{
 const f=fixture({loseAdmission:true});await f.ready();await f.manager.submit(f.tab,f.node);
 assert.equal(f.records.size,1);const pending=agentDocument(f.node,f.project).pending;assert.ok(pending);
 f.node.content=f.stored[0];f.offline=false;f.tab.workspace={status:'stopped'};f.calls.length=0;
 const reloaded=new FolderAgents(f.host,f.api);await reloaded.refresh(f.tab,f.node);
 assert.equal(reloaded.get(f.tab,f.node).task.id,pending.id);assert.equal(agentDocument(f.node,f.project).taskId,pending.id);assert.equal(f.calls.filter(c=>c.method!=='GET').length,0);
});
test('saved requests keep their IDs and body when explicitly retried',async()=>{
 const f=fixture();await f.ready();f.offline=true;await f.manager.submit(f.tab,f.node);const pending=agentDocument(f.node,f.project).pending;assert.ok(pending);
 f.offline=false;f.calls.length=0;await f.manager.submit(f.tab,f.node);
 const admitted=f.calls.find(c=>c.method==='PUT'&&c.path.includes('/tasks/'));assert.ok(admitted.path.endsWith(pending.id));assert.deepEqual(admitted.body,pending.request);
});
test('unverified or disconnected saved selections never fall back to another model',async()=>{
 for(const options of [{unverified:true},{otherModel:true}]){
  const f=fixture(options);await f.ready();const selected=selectionKey(agentDocument(f.node,f.project).selection);await f.manager.submit(f.tab,f.node);
  assert.equal(f.calls.length,0);assert.equal(selectionKey(agentDocument(f.node,f.project).selection),selected);
 }
});
test('changing the private draft during an accepted request preserves the new draft',async()=>{
 const f=fixture();await f.ready();let release;f.gate=new Promise(r=>release=r);
 const submitting=f.manager.submit(f.tab,f.node);f.manager.change(f.tab,f.node,{draft:'Next unsent request'});release();await submitting;
 assert.equal(agentDocument(f.node,f.project).draft,'Next unsent request');assert.equal([...f.records.values()][0].request.input,'Update saved files');
});
test('removing a panel while its save is pending stops dispatch and keeps project task ownership separate',async()=>{
 const f=fixture();await f.ready();let release;f.gate=new Promise(r=>release=r);const submitting=f.manager.submit(f.tab,f.node);f.owned=false;release();await submitting;
 assert.equal(f.calls.length,0);assert.equal(f.records.size,0);
});
test('saved scope corruption prevents a request from being retargeted',async()=>{
 const f=fixture();await f.ready();const d=agentDocument(f.node,f.project);d.pending={id:randomUUID(),snapshot:randomUUID(),request:{kind:'code',input:'wrong',projectDirectory:'other',selection:d.selection,coordination:{assignedAgent:agentLabel(f.node)},instructionSnapshotId:randomUUID()}};f.node.content=JSON.stringify(d);
 assert.equal(agentDocument(f.node,f.project),undefined);
});

test('corrupt saved sessions never probe providers, refresh tasks, or dispatch',async()=>{
 const f=fixture();f.node.content='{"schema":999}';
 await f.manager.models(f.tab,f.node,true);await f.manager.refresh(f.tab,f.node);await f.manager.submit(f.tab,f.node);
 assert.equal(f.calls.length,0);assert.equal(f.stored.length,0);
});
test('closing an agent panel clears its cache without cancelling its project tasks',async()=>{
 const f=fixture();await f.ready();await f.manager.submit(f.tab,f.node);assert.equal(f.records.size,1);
 const old=f.manager.get(f.tab,f.node);f.manager.forget(f.tab,f.node.id);
 assert.notEqual(f.manager.get(f.tab,f.node),old);assert.equal(f.records.size,1);
 assert.equal(f.calls.filter(c=>c.path.includes('/cancel')).length,0);
});

test('surrounding request whitespace matches server normalization without an ambiguous accepted task',async()=>{
 const f=fixture();await f.ready();f.manager.change(f.tab,f.node,{draft:'  Update saved files\n'});
 await f.manager.submit(f.tab,f.node);
 assert.equal(f.records.size,1);assert.equal([...f.records.values()][0].request.input,'Update saved files');
 assert.equal(agentDocument(f.node,f.project).pending,undefined);assert.equal(agentDocument(f.node,f.project).draft,'');
});
