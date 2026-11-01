import {test} from 'node:test';
import assert from 'node:assert/strict';

async function fixture(t) {
  const original={window:globalThis.window,document:globalThis.document,ResizeObserver:globalThis.ResizeObserver};
  const listeners=new Map(),frames=[];
  globalThis.window={addEventListener:(name,fn)=>listeners.set(name,fn)};
  globalThis.document={dispatchEvent(){},body:{appendChild:frame=>frames.push(frame)},getElementById:()=>({getBoundingClientRect:()=>({left:0,top:0,width:800,height:600})}),
    createElement:()=>({dataset:{},style:{},attributes:new Map(),writes:0,removed:false,inert:false,
      setAttribute(name,value){this.attributes.set(name,value);},getAttribute(name){return this.attributes.get(name)??null;},
      get src(){return this.attributes.get('src');},set src(value){this.attributes.set('src',value);this.writes++;},contentWindow:{postMessage(){}},remove(){this.removed=true;},blur(){this.blurred=true;}})};
  globalThis.ResizeObserver=class{observe(){}disconnect(){}};
  const module=await import(`../web/canvas-panes.js?fixture=${Math.random()}`);
  t.after(()=>Object.assign(globalThis,original));
  const call=(action,payload)=>module.canvasPanes(action,JSON.stringify(payload));
  const attach=(id='tab-a',client='client-a',url='http://fixture.editor.localhost/?firecrabClient=client-a')=>call('attach',{surfaces:[],keep:[],editor:{id,client,url}});
  const proof='55000000-0000-4000-8000-000000000099';
  const activate=frame=>{frame.onload();listeners.get('message')({source:frame.contentWindow,origin:new URL(frame.src).origin,data:{type:'firecrab-window-proof',client:frame.dataset.client,windowProof:proof,challenge:frame.challenge}});};
  return {call,attach,frames,listeners,activate,proof};
}

test('rendering and hiding an editor retain its existing document without reloading it',async t=>{
  const f=await fixture(t);f.attach();const frame=f.frames[0];
  f.call('prepare',{});assert.equal(frame.hidden,true);f.attach();
  assert.equal(f.frames.length,1);assert.equal(frame.writes,1);assert.equal(frame.removed,false);
  assert.equal(f.call('can-attach-editor',{id:'tab-a',client:'client-b',url:'http://other.editor.localhost/'}),false);
  assert.equal(f.attach('tab-a','client-b','http://other.editor.localhost/'),false);
  assert.equal(frame.writes,1);assert.equal(frame.removed,false);
});

test('only the matching confirmed client can detach its frame, while another tab stays connected',async t=>{
  const f=await fixture(t);f.attach();f.attach('tab-b','client-b','http://fixture.editor.localhost/?firecrabClient=client-b');
  const [a,b]=f.frames;f.activate(a);f.activate(b);
  assert.equal(f.call('freeze-editor',{id:'tab-a',client:'client-a',value:true}),true);assert.equal(a.inert,true);assert.equal(a.blurred,true);
  assert.equal(f.call('detach-editor',{id:'tab-a',client:'stale-client'}),false);assert.equal(a.removed,false);
  assert.equal(f.call('freeze-editor',{id:'tab-a',client:'client-a',windowProof:f.proof,value:false}),true);assert.equal(a.inert,false);
  assert.equal(f.call('detach-editor',{id:'tab-a',client:'client-a',windowProof:f.proof}),true);assert.equal(a.removed,true);assert.equal(b.removed,false);
  assert.equal(f.call('has-editor',{id:'tab-a'}),false);assert.equal(f.call('has-editor',{id:'tab-b'}),true);
});

test('a page unload warns while an editor surface remains and clears after confirmed disposal',async t=>{
  const f=await fixture(t);f.attach();f.activate(f.frames[0]);let prevented=0;
  const event={preventDefault(){prevented++;}};f.listeners.get('beforeunload')(event);
  assert.equal(prevented,1);assert.equal(event.returnValue,'');
  f.call('detach-editor',{id:'tab-a',client:'client-a',windowProof:f.proof});
  f.listeners.get('beforeunload')({preventDefault(){prevented++;}});assert.equal(prevented,1);
});


test('only the current iframe document can publish its proof, and a reload invalidates old acknowledgements',async t=>{
  const f=await fixture(t);f.attach();const frame=f.frames[0];f.activate(frame);
  assert.equal(f.call('editor-proof',{id:'tab-a',client:'client-a'}),f.proof);
  const old=frame.challenge;frame.onload();
  const message={type:'firecrab-window-proof',client:'client-a',windowProof:f.proof,challenge:old};
  for(const event of [
    {source:frame.contentWindow,origin:new URL(frame.src).origin,data:message},
    {source:{},origin:new URL(frame.src).origin,data:{...message,challenge:frame.challenge}},
    {source:frame.contentWindow,origin:'http://other.editor.localhost',data:{...message,challenge:frame.challenge}},
  ])f.listeners.get('message')(event);
  assert.equal(f.call('editor-proof',{id:'tab-a',client:'client-a'}),null);
  const next='55000000-0000-4000-8000-000000000088';
  f.listeners.get('message')({source:frame.contentWindow,origin:new URL(frame.src).origin,data:{...message,windowProof:next,challenge:frame.challenge}});
  assert.equal(f.call('detach-editor',{id:'tab-a',client:'client-a',windowProof:f.proof}),false);
  assert.equal(frame.removed,false);
  assert.equal(f.call('detach-editor',{id:'tab-a',client:'client-a',windowProof:next}),true);
});

test('the workbench proof bridge answers only its real parent and preserves the challenge',async()=>{
  const fs=await import('node:fs'),vm=await import('node:vm');const sent=[];let receive;
  const parent={postMessage:(value,origin)=>sent.push({value,origin})};
  const defaults={'firecrab.browserClientId':'client','firecrab.browserWindowProof':'proof','firecrab.parentOrigin':'http://parent.localhost'};
  vm.runInNewContext(fs.readFileSync(new URL('../../firecrab-api/src/project_workspace/editors/editor/browser-bridge.js',import.meta.url),'utf8'),{
    document:{getElementById:()=>({getAttribute:()=>JSON.stringify({configurationDefaults:defaults})})},window:{parent,addEventListener:(_,fn)=>receive=fn},
  });
  const data={type:'firecrab-window-proof',client:'client',challenge:'fresh challenge'};
  receive({source:{},origin:'http://parent.localhost',data});receive({source:parent,origin:'http://other.localhost',data});
  assert.equal(sent.length,0);receive({source:parent,origin:'http://parent.localhost',data});
  assert.equal(sent.length,1);assert.equal(sent[0].value.windowProof,'proof');assert.equal(sent[0].value.challenge,'fresh challenge');assert.equal(sent[0].origin,'http://parent.localhost');
});
