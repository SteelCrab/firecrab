import {test} from 'node:test';
import assert from 'node:assert/strict';
import {request} from '../src/canvas/request.ts';
import {backupKey,readBackupIntent,cleanupKey,readCleanupIntent} from '../src/canvas/backups.ts';
const project='55000000-0000-4000-8000-0000000000ac';
test('polling a restored workspace does not silently adopt its generation for stale writes',async()=>{
 const original=globalThis.fetch,calls=[];let generation=1,restored=false;
 globalThis.fetch=async(path,options)=>{calls.push({path,options});return Response.json({generation,writeGenerationRequired:restored});};
 try {
  const path=`/api/ai-sandbox/projects/${project}/workspace`;
  await request('GET',path);await request('PUT',path+'/state',{});assert.equal(calls.at(-1).options.headers['X-Firecrab-Workspace-Generation'],'1');
  generation=2;restored=true;await request('GET',path);await request('PUT',path+'/views/example',{});assert.equal(calls.at(-1).options.headers['X-Firecrab-Workspace-Generation'],'1');
  await request('PUT',`/api/ai-sandbox/projects/${project}/tasks/example`,{});assert.equal(calls.at(-1).options.headers['X-Firecrab-Workspace-Generation'],'1');
  const other=`/api/ai-sandbox/projects/${project.replace(/ac$/,'ad')}/workspace`;
  await request('GET',other);await request('PUT',other+'/state',{});assert.equal(calls.at(-1).options.headers['X-Firecrab-Workspace-Generation'],'2');
 }finally{globalThis.fetch=original;}
});
test('backup retries retain the exact intent and reject malformed persisted requests',()=>{
 const intent={id:project,expectedGeneration:7,restoreCheckpoint:null};
 const values=new Map([[backupKey(project),JSON.stringify(intent)]]),storage={getItem:key=>values.get(key)??null};
 assert.deepEqual(readBackupIntent(storage,project),intent);assert.equal(readBackupIntent(storage,'other'),undefined);
 for(const value of [{...intent,id:'../../other'},{...intent,expectedGeneration:0},{...intent,restoreCheckpoint:'bad'}]) {values.set(backupKey(project),JSON.stringify(value));assert.throws(()=>readBackupIntent(storage,project));}
});

test('cleanup recovery retains an exact bounded target set and rejects malformed saved deletion requests',()=>{
 const intent={id:project,expectedGeneration:3,targetIds:[project]};const values=new Map([[cleanupKey(project),JSON.stringify(intent)]]),storage={getItem:key=>values.get(key)??null};
 assert.deepEqual(readCleanupIntent(storage,project),intent);assert.equal(readCleanupIntent(storage,'other'),undefined);
 for(const value of [{...intent,targetIds:[]},{...intent,targetIds:[project,project]},{...intent,targetIds:Array(65).fill(project)},{...intent,id:'../../elsewhere'},{...intent,expectedGeneration:0},{...intent,targetIds:['00000000-0000-0000-0000-000000000000']}]) {values.set(cleanupKey(project),JSON.stringify(value));assert.throws(()=>readCleanupIntent(storage,project));}
});
