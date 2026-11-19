// Windows browser regression. Uses the installed Edge and Vite; no external model or user data.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdir, writeFile, access } from 'node:fs/promises';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { randomUUID } from 'node:crypto';

const root = resolve(import.meta.dirname,'..');
const scratch = resolve(root,'.build-tmp',`context-budget-ui-${randomUUID()}`);
const edgePath = process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
await access(edgePath);
await mkdir(scratch,{recursive:true});
const freePort = () => new Promise((resolve,reject) => {
  const server = createServer();
  server.once('error',reject);
  server.listen(0,'127.0.0.1',() => { const port = server.address().port; server.close(() => resolve(port)); });
});
const port = await freePort();
const cdpPort = await freePort();
const html = resolve(scratch,'index.html');
await writeFile(html,'<!doctype html><html data-theme="glass"><head><title>Context budget fixture</title></head><body><div id="fixture"></div><script type="module" src="/scripts/fixtures/context-budget-ui.tsx"></script></body></html>');
let logs = '';
const vite = spawn(process.execPath,[resolve(root,'node_modules/vite/bin/vite.js'),'--host','127.0.0.1','--port',String(port),'--strictPort'],{cwd:root,windowsHide:true,stdio:['ignore','pipe','pipe']});
vite.stdout.on('data',data => { logs += data; });
vite.stderr.on('data',data => { logs += data; });
let edge;
let socket;
try {
  const url = `http://127.0.0.1:${port}/${html.slice(root.length+1).replaceAll('\\','/')}`;
  let ready = false;
  for (const deadline=Date.now()+20000;Date.now()<deadline;) {
    try { ready = (await fetch(url)).ok; } catch {}
    if (ready) break;
    if (vite.exitCode != null) throw new Error(logs);
    await delay(200);
  }
  assert(ready,`Vite did not start: ${logs}`);
  edge = spawn(edgePath,['--headless=new','--disable-gpu','--no-first-run',`--remote-debugging-port=${cdpPort}`,`--user-data-dir=${resolve(scratch,'edge')}`,url],{windowsHide:true,stdio:'ignore'});
  let target;
  for (const deadline=Date.now()+30000;Date.now()<deadline;) {
    try { target=(await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json()).find(tab => tab.url === url); } catch {}
    if (/^(PASS|FAIL)/.test(target?.title || '')) break;
    await delay(250);
  }
  assert(target?.title.startsWith('PASS'),target?.title || `Browser fixture failed to load: ${logs}`);
  socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve,reject) => { socket.onopen=resolve; socket.onerror=reject; });
  let seq=0;
  const pending=new Map();
  socket.onmessage=({data}) => { const response=JSON.parse(data); const item=pending.get(response.id); if(item){pending.delete(response.id);clearTimeout(item.timer);response.error ? item.reject(new Error(JSON.stringify(response.error))) : item.resolve(response.result);} };
  const command=(method,params={}) => new Promise((resolve,reject) => {
    const id=++seq;
    const timer=setTimeout(() => { pending.delete(id); reject(new Error(`${method} timed out`)); },10000);
    pending.set(id,{resolve,reject,timer});
    socket.send(JSON.stringify({id,method,params}));
  });
  const state = (await command('Runtime.evaluate',{expression:'({title:document.title, overlay:Boolean(document.querySelector("vite-error-overlay")), meaningful:document.body.textContent.includes("48,000")})',returnByValue:true})).result.value;
  assert(state.meaningful && !state.overlay,'Fixture renders meaningful content without framework errors');
  const shot=await command('Page.captureScreenshot',{format:'png',captureBeyondViewport:true});
  const screenshot=resolve(scratch,'context-budget.png');
  await writeFile(screenshot,Buffer.from(shot.data,'base64'));
  console.log(state.title);
  console.log(`Screenshot: ${screenshot}`);
} finally {
  socket?.close();
  edge?.kill();
  vite.kill();
}
