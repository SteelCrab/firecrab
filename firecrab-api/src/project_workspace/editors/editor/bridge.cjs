// Fixed transport adapter. Drop root before opening an actor-owned socket.
const fs=require('node:fs');
const net=require('node:net');
const target=process.argv[1],uid=Number(process.argv[2]);
if(process.getuid()!==0||!Number.isInteger(uid)||uid<100000||!/^\/workspace\/shared\/\.firecrab-workspace\/editors\/[0-9a-f-]{36}\/data\/server\.sock$/.test(target)) throw Error('Invalid editor channel');
const gid=fs.statSync('/workspace/shared').gid;
process.setgroups([]);process.setgid(gid);process.setuid(uid);
const stat=fs.lstatSync(target);
if(!stat.isSocket()||stat.uid!==uid) throw Error('Editor socket is not owned by this actor');
const socket=net.createConnection(target);
socket.on('error',()=>process.exit(1));
process.stdin.on('error',()=>socket.destroy());
process.stdout.on('error',()=>socket.destroy());
process.stdin.pipe(socket);socket.pipe(process.stdout);
socket.on('close',()=>process.exit(0));
