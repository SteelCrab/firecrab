import './styles/console.css';
import './styles/ssh.css';
import { createWasmTerminal } from './terminal/engine.js';

const status=document.getElementById('terminal-status');
const controller=new AbortController();
const socket=new WebSocket(`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/_fc/terminal/socket`);
socket.binaryType='arraybuffer';
const encoder=new TextEncoder();
const options={fontSize:14,fontFamily:'ui-monospace, monospace',theme:{background:'#151515',foreground:'#eeeeee',cursor:'#ffffff',selectionBackground:'#444444'}};
let session,ended=false,pending=[],pendingBytes=0;
const closed=()=>{if(ended)return;ended=true;status.textContent='Disconnected. Return to the workspace to reconnect.';if(session?.terminal.textarea)session.terminal.textarea.disabled=true;socket.close();};
const opened=()=>{if(ended)return;status.textContent='/home/fc-code/project';session?.reportDimensions();session?.terminal.focus();};
socket.addEventListener('open',opened);
socket.addEventListener('message',event=>{const bytes=new Uint8Array(event.data);if(session)session.write(bytes);else{pendingBytes+=bytes.length;if(pendingBytes>1024*1024){closed();return;}pending.push(bytes);}});
socket.addEventListener('close',closed,{once:true});socket.addEventListener('error',closed,{once:true});
try{
  session=await createWasmTerminal(document.getElementById('terminal'),options,controller.signal,text=>{document.getElementById("terminal-output").textContent=text;},closed,
    bytes=>{if(socket.readyState!==WebSocket.OPEN)return;const data=typeof bytes==='string'?encoder.encode(bytes):bytes;for(let offset=0;offset<data.length;offset+=16384)socket.send(data.subarray(offset,offset+16384));},
    size=>{if(socket.readyState===WebSocket.OPEN)socket.send(JSON.stringify(size));});
  for(const bytes of pending)session.write(bytes);pending=[];
  if(socket.readyState===WebSocket.OPEN)opened();
}catch{closed();}
document.getElementById('terminal-close').addEventListener('click',closed,{once:true});
window.addEventListener('pagehide',()=>{closed();controller.abort();session?.dispose();},{once:true});
