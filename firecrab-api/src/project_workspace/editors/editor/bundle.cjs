// Trusted bootstrap only. Publish a complete code revision without modifying old bundles.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const ROOT = '/workspace/shared/.firecrab-workspace/editor-package';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const SHA = /^[0-9a-f]{64}$/;
function digest(archiveSha, manifest, extension) {
  const hash = crypto.createHash('sha256').update('firecrab-editor-bundle-v1\0').update(archiveSha);
  for (const bytes of [manifest, extension]) {
    const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(bytes.length));
    hash.update(length).update(bytes);
  }
  return hash.digest('hex');
}
function publish(config, root = ROOT, owner = 0) {
  const {base, archiveSha, revision, nonce} = config;
  if (path.dirname(base) !== root || !['openvscode-server-v1.109.5-linux-arm64','openvscode-server-v1.109.5-linux-x64'].includes(path.basename(base))
      || archiveSha.length !== 64 || !SHA.test(archiveSha) || revision.length !== 64 || !SHA.test(revision)
      || nonce.length !== 36 || !UUID.test(nonce)) throw Error('Invalid editor bundle identity');
  const manifest = Buffer.from(config.manifest, 'base64'), extension = Buffer.from(config.extension, 'base64');
  if (manifest.length > 65536 || extension.length > 262144 || digest(archiveSha, manifest, extension) !== revision) throw Error('Invalid editor bundle contents');
  function stat(name, directory, mode) {
    const value = fs.lstatSync(name);
    if (value.uid !== owner || (directory ? !value.isDirectory() : !value.isFile())
        || (value.mode & 0o777) !== mode) throw Error('Unexpected editor code ownership or mode');
    return value;
  }
  function read(name) {
    const fd = fs.openSync(name, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    try {
      const value = fs.fstatSync(fd);
      if (!value.isFile() || value.uid !== owner || (value.mode & 0o022)) throw Error('Unexpected editor code file');
      return fs.readFileSync(fd);
    } finally { fs.closeSync(fd); }
  }
  stat(root, true, 0o755); stat(base, true, 0o755);
  const node = stat(path.join(base, 'node'), false, 0o755);
  if (read(path.join(base, 'firecrab-archive-sha256')).toString() !== archiveSha) throw Error('Unexpected editor archive identity');
  const bundles = path.join(root, 'bundles');
  if (!fs.existsSync(bundles)) fs.mkdirSync(bundles, {mode:0o755});
  stat(bundles, true, 0o755);
  const target = path.join(bundles, revision);
  const custom = 'extensions/firecrab-project-workspace';
  function verify() {
    stat(target, true, 0o755);
    const installedNode = stat(path.join(target, 'node'), false, 0o755);
    if (node.dev !== installedNode.dev || node.ino !== installedNode.ino
        || read(path.join(target, 'firecrab-archive-sha256')).toString() !== archiveSha
        || read(path.join(target, 'firecrab-bundle-sha256')).toString() !== revision) throw Error('Unexpected published editor bundle');
    stat(path.join(target, 'extensions'), true, 0o755);
    stat(path.join(target, custom), true, 0o755);
    if (!read(path.join(target, custom, 'package.json')).equals(manifest)
        || !read(path.join(target, custom, 'extension.js')).equals(extension)) throw Error('Published editor bridge changed');
    return target;
  }
  if (fs.existsSync(target)) return verify();
  const stage = path.join(bundles, `stage-${nonce}`);
  // This invocation owns only this new stage. An interrupted older stage is retained.
  fs.mkdirSync(stage, {mode:0o755});
  try {
    let entries = 0;
    function copy(source, destination) {
      if (++entries > 250000) throw Error('Editor package entry limit exceeded');
      const value = fs.lstatSync(source);
      if (value.uid !== owner || (!value.isSymbolicLink() && (value.mode & 0o022))) throw Error('Editor base is writable by another user');
      if (value.isDirectory()) {
        fs.mkdirSync(destination, {mode:value.mode & 0o777});
        for (const name of fs.readdirSync(source)) {
          const child = path.join(source, name);
          if (child === path.join(base, custom)) continue;
          copy(child, path.join(destination, name));
        }
        syncDirectory(destination);
      } else if (value.isFile()) {
        // Link immutable official files. Never write, chmod, or truncate their shared inodes.
        fs.linkSync(source, destination);
      } else if (value.isSymbolicLink()) {
        const link = fs.readlinkSync(source), resolved = path.resolve(path.dirname(source), link);
        if (!resolved.startsWith(base + path.sep)) throw Error('Editor base symlink leaves its package');
        fs.symlinkSync(link, destination);
      } else throw Error('Unexpected editor base entry');
    }
    for (const name of fs.readdirSync(base)) copy(path.join(base, name), path.join(stage, name));
    stat(path.join(stage, 'extensions'), true, 0o755);
    fs.mkdirSync(path.join(stage, custom), {mode:0o755});
    function write(name, bytes) {
      const fd = fs.openSync(name, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o644);
      try { fs.writeFileSync(fd, bytes); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
    }
    function syncDirectory(name) {
      const fd = fs.openSync(name, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW);
      try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
    }
    write(path.join(stage, custom, 'package.json'), manifest);
    write(path.join(stage, custom, 'extension.js'), extension);
    write(path.join(stage, 'firecrab-bundle-sha256'), revision);
    syncDirectory(path.join(stage, custom)); syncDirectory(path.join(stage, 'extensions')); syncDirectory(stage);
    try { fs.renameSync(stage, target); }
    catch (failure) { if (!['EEXIST', 'ENOTEMPTY'].includes(failure.code)) throw failure; }
    syncDirectory(bundles);
    return verify();
  } finally { if (fs.existsSync(stage)) fs.rmSync(stage, {recursive:true}); }
}
if (require.main === module || module.id === '[stdin]') {
  if (process.getuid() !== 0) throw Error('Editor bundle publication requires the trusted bootstrap');
  const metadata = fs.lstatSync(path.dirname(ROOT));
  if (!metadata.isDirectory() || metadata.uid !== 0 || (metadata.mode & 0o022)) throw Error('Invalid project metadata directory');
  process.umask(0o022);
  publish(JSON.parse(Buffer.from(process.argv[2], 'base64').toString('utf8')));
  console.log('published');
}
module.exports = {publish, digest};
