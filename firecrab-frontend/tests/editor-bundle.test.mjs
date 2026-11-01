import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {spawnSync} from 'node:child_process';
import bundle from '../../firecrab-api/src/project_workspace/editors/editor/bundle.cjs';

function fixture(t) {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'firecrab-editor-bundle-'));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  fs.chmodSync(root,0o755);
  const base=path.join(root,'openvscode-server-v1.109.5-linux-arm64');
  fs.mkdirSync(base,{mode:0o755}); fs.mkdirSync(path.join(base,'extensions'),{mode:0o755});
  fs.mkdirSync(path.join(base,'extensions','firecrab-project-workspace'),{mode:0o755});
  fs.writeFileSync(path.join(base,'extensions','firecrab-project-workspace','extension.js'),'legacy bridge',{mode:0o644});
  fs.writeFileSync(path.join(base,'node'),'official native executable',{mode:0o755});
  fs.writeFileSync(path.join(base,'firecrab-archive-sha256'),'a'.repeat(64),{mode:0o644});
  const manifest=Buffer.from('{"name":"project-workspace"}');
  function config(source) {
    const extension=Buffer.from(source), archiveSha='a'.repeat(64);
    return {base,archiveSha,revision:bundle.digest(archiveSha,manifest,extension),nonce:randomUUID(),manifest:manifest.toString('base64'),extension:extension.toString('base64')};
  }
  const publish=config=>bundle.publish(config,root,process.getuid());
  return {root,base,config,publish};
}

test('two editor revisions and the legacy bridge coexist without replacing shared code',t=>{
  const f=fixture(t), first=f.config('bridge one'), second=f.config('bridge two');
  const one=f.publish(first), before=fs.statSync(path.join(one,'extensions','firecrab-project-workspace','extension.js'));
  const two=f.publish(second);
  assert.notEqual(one,two);
  assert.equal(fs.readFileSync(path.join(one,'extensions','firecrab-project-workspace','extension.js'),'utf8'),'bridge one');
  assert.equal(fs.readFileSync(path.join(two,'extensions','firecrab-project-workspace','extension.js'),'utf8'),'bridge two');
  assert.equal(fs.readFileSync(path.join(f.base,'extensions','firecrab-project-workspace','extension.js'),'utf8'),'legacy bridge');
  assert.equal(fs.statSync(path.join(one,'node')).ino,fs.statSync(path.join(f.base,'node')).ino);
  assert.equal(fs.statSync(path.join(two,'node')).ino,fs.statSync(path.join(f.base,'node')).ino);
  assert.equal(f.publish({...first,nonce:randomUUID()}),one);
  assert.equal(fs.statSync(path.join(one,'extensions','firecrab-project-workspace','extension.js')).ino,before.ino);
});

test('an interrupted stage is not published or reused; a fresh attempt preserves its evidence',t=>{
  const f=fixture(t), config=f.config('new bridge');
  fs.mkdirSync(path.join(f.root,'bundles'),{mode:0o755});
  const interrupted=path.join(f.root,'bundles',`stage-${config.nonce}`);
  fs.mkdirSync(interrupted,{mode:0o755});fs.writeFileSync(path.join(interrupted,'partial'),'partial code');
  assert.throws(()=>f.publish(config),/EEXIST/);
  assert.equal(fs.existsSync(path.join(f.root,'bundles',config.revision)),false);
  f.publish({...config,nonce:randomUUID()});
  assert.equal(fs.readFileSync(path.join(interrupted,'partial'),'utf8'),'partial code');
});

test('tampered revisions fail without replacing an existing editor package',t=>{
  const f=fixture(t), config=f.config('original bridge'), target=f.publish(config);
  const bridge=path.join(target,'extensions','firecrab-project-workspace','extension.js');
  fs.writeFileSync(bridge,'tampered bridge');
  assert.throws(()=>f.publish({...config,nonce:randomUUID()}),/bridge changed/);
  assert.equal(fs.readFileSync(bridge,'utf8'),'tampered bridge');
  assert.equal(fs.readFileSync(path.join(f.base,'extensions','firecrab-project-workspace','extension.js'),'utf8'),'legacy bridge');
});

test('unexpected code ownership, symlinks, or writable base files cannot enter a bundle',t=>{
  const f=fixture(t), config=f.config('new bridge');
  assert.throws(()=>bundle.publish(config,f.root,process.getuid()+1),/ownership/);
  fs.chmodSync(path.join(f.base,'node'),0o777);
  assert.throws(()=>f.publish(config),/ownership or mode/);
  fs.chmodSync(path.join(f.base,'node'),0o755);
  fs.symlinkSync('/etc/passwd',path.join(f.base,'outside'));
  assert.throws(()=>f.publish(config),/symlink leaves/);
  assert.equal(fs.existsSync(path.join(f.root,'bundles',config.revision)),false);
  assert.deepEqual(fs.readdirSync(path.join(f.root,'bundles')),[]);
});

test('a corrupted content digest and path injection fail before any publication',t=>{
  const f=fixture(t), config=f.config('bridge');
  for(const changed of [{revision:'b'.repeat(64)},{nonce:'../node'},{base:'/tmp/node'},{archiveSha:'"; id'},{nonce:config.nonce+'\n'},{revision:config.revision+'\n'},{archiveSha:config.archiveSha+'\n'},{base:config.base+'\n'}]) {
    assert.throws(()=>f.publish({...config,...changed}),/Invalid editor bundle/);
  }
  assert.equal(fs.existsSync(path.join(f.root,'bundles')),false);
});

test('the stdin bootstrap executes its root gate instead of silently skipping publication',()=>{
  const script=fs.readFileSync(new URL('../../firecrab-api/src/project_workspace/editors/editor/bundle.cjs',import.meta.url),'utf8');
  const result=spawnSync(process.execPath,['-','e30='],{input:script,encoding:'utf8'});
  assert.notEqual(result.status,0);
  if(process.getuid()!==0) assert.match(result.stderr,/requires the trusted bootstrap/);
});

test('the Node digest matches the Rust length-delimited byte contract',()=>{
  assert.equal(bundle.digest('a'.repeat(64),Buffer.from('{}'),Buffer.from('bridge')),'3736b00bf8f2964eaac9007f1559b25e76983450bb9e8da0d36ea93f0e93bddc');
});

test('published bundles execute their own immutable revision through a real Node binary',t=>{
  const f=fixture(t);
  fs.copyFileSync(process.execPath,path.join(f.base,'node'));
  fs.chmodSync(path.join(f.base,'node'),0o755);
  const one=f.publish(f.config("module.exports='revision one';"));
  const two=f.publish(f.config("module.exports='revision two';"));
  for(const [target,expected] of [[one,'revision one'],[two,'revision two']]) {
    const file=path.join(target,'extensions','firecrab-project-workspace','extension.js');
    // Homebrew's Node links libnode relative to its installed executable. Keep
    // that test-only library lookup when executing the copied binary on macOS.
    const env=process.platform==='darwin'
      ? {...process.env,DYLD_FALLBACK_LIBRARY_PATH:path.resolve(path.dirname(process.execPath),'../lib')}
      : process.env;
    const result=spawnSync(path.join(target,'node'),['-e','process.stdout.write(require(process.argv[1]))',file],{encoding:'utf8',env});
    assert.equal(result.status,0,result.stderr);
    assert.equal(result.stdout,expected);
  }
});

test('failed atomic publication leaves the previous revision executable and clears only its own stage',t=>{
  const f=fixture(t), first=f.config('original bridge'), one=f.publish(first), next=f.config('new bridge');
  const rename=fs.renameSync;
  fs.renameSync=(source,target)=>{
    if(target===path.join(f.root,'bundles',next.revision)) throw Object.assign(Error('simulated publication I/O failure'),{code:'EIO'});
    return rename(source,target);
  };
  try { assert.throws(()=>f.publish(next),/publication I\/O failure/); }
  finally { fs.renameSync=rename; }
  assert.deepEqual(fs.readdirSync(path.join(f.root,'bundles')),[first.revision]);
  assert.equal(fs.readFileSync(path.join(one,'extensions','firecrab-project-workspace','extension.js'),'utf8'),'original bridge');
  assert.equal(f.publish({...next,nonce:randomUUID()}),path.join(f.root,'bundles',next.revision));
});
