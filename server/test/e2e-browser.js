import test from 'node:test';
import assert from 'node:assert/strict';
import { fork, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import puppeteer from 'puppeteer-core';
import WebSocket from 'ws';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(action, description, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await action(); if (result) return result; await sleep(50); }
  throw new Error(`Timed out: ${description}`);
}
async function deadline(promise, ms, message) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), ms); })]); }
  finally { clearTimeout(timer); }
}
function alive(pid) { try { process.kill(pid, 0); return true; } catch (e) { if (e.code === 'ESRCH') return false; throw e; } }
async function viewerPath() {
  for (const path of [process.env.PRTS_BROWSER, 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe'].filter(Boolean)) {
    try { await access(path); return path; } catch {}
  }
  throw new Error('Set PRTS_BROWSER to an installed Chrome/Edge viewer');
}

test('PRTS browser frames: real Chromium page pixels, input, targets and two instances', { timeout: 420000 }, async t => {
  const fixture = createServer((req, res) => {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    const blue = req.url.includes('blue'), staticPage = req.url.includes('static');
    res.end(`<!doctype html><style>body{margin:0;background:rgb(${blue ? '20,60,210' : '210,35,60'})}button{position:absolute;left:40px;top:40px;width:120px;height:60px}input{position:absolute;left:40px;top:160px;width:280px;height:60px}#text{position:absolute;left:450px;top:50px;width:80px;height:80px;background:rgb(220,150,20)}#motion{position:absolute;left:450px;top:300px;width:80px;height:80px;background:white}</style>
      <button onclick="document.body.style.background='rgb(20,190,60)';document.title='clicked'">Click</button>
      <input oninput="document.title='typed:'+this.value;document.querySelector('#text').style.background='rgb(20,60,220)'">
      <div id=text></div><div id=motion></div>
      <script>document.title=location.hostname+':fixture';${staticPage ? '' : "let n=0;setInterval(()=>document.querySelector('#motion').style.background=++n%2?'rgb(240,240,240)':'rgb(20,20,20)',33)"}</script>`);
  });
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening');
  const url = `http://127.0.0.1:${fixture.address().port}/`;
  const daemonPath = process.env.PRTSD_PATH || fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe', import.meta.url));
  const server = fork(process.env.PRTS_TEST_SERVER_DIR ? join(process.env.PRTS_TEST_SERVER_DIR,'index.js') : fileURLToPath(new URL('../index.js', import.meta.url)), [], {
    cwd: fileURLToPath(new URL('../../', import.meta.url)), windowsHide: true, silent: true,
    env: { ...process.env, PORT: '0', PRTSD_PATH: daemonPath, PRTSD_ARGS: '[]', PRTS_BROWSER_PROTOCOL:'1' },
  });
  let logs = '', browser, daemonPid;
  const exit = once(server, 'exit');
  server.stdout.on('data', d => { logs += d; }); server.stderr.on('data', d => { logs += d; });
  t.after(async () => {
    await browser?.close(); fixture.close();
    if (server.connected) server.disconnect();
    await deadline(exit, 10000, `Server shutdown timed out\n${logs}`).catch(e => { server.kill(); throw e; });
    if (daemonPid) await until(() => !alive(daemonPid), 'exact daemon PID exit', 8000);
    assert.equal(server.exitCode, 0, logs);
    console.log(logs);
  });
  const [ready] = await deadline(once(server, 'message'), 15000, 'Node server startup'); daemonPid = ready.daemonPid;
  browser = await puppeteer.launch({ executablePath: await viewerPath(), headless: true, args: ['--autoplay-policy=no-user-gesture-required'], defaultViewport: { width:1280,height:1000 } });
  const errors = [];
  async function page() {
    const p = await browser.newPage(); p.on('pageerror', e => errors.push(e.message));
    await p.goto(ready.url); await p.waitForFunction(() => document.querySelector('#status').textContent.includes('connected'));
    return p;
  }
  async function field(p, selector, value) { await p.$eval(selector, (e, v) => { e.value = v; }, String(value)); }
  async function pixel(p, x, y) {
    return p.$eval('video', (v, [x,y]) => {
      if (!v.videoWidth || v.readyState < 2) return [0,0,0];
      const c = document.createElement('canvas'); c.width=v.videoWidth; c.height=v.videoHeight;
      const ctx=c.getContext('2d'); ctx.drawImage(v,0,0); return [...ctx.getImageData(x,y,1,1).data].slice(0,3);
    }, [x,y]);
  }
  async function color(p, x, y, rgb, description) {
    await until(async () => (await pixel(p,x,y)).every((v,i) => Math.abs(v-rgb[i]) < 45), description);
  }
  async function remoteClick(p,x,y) {
    await p.$eval('video', v => v.scrollIntoView({block:'center'}));
    const point = await p.$eval('video',(v,[x,y]) => {
      const r=v.getBoundingClientRect(), s=Math.min(r.width/v.videoWidth,r.height/v.videoHeight);
      return {x:r.x+(r.width-v.videoWidth*s)/2+x*s,y:r.y+(r.height-v.videoHeight*s)/2+y*s};
    },[x,y]);
    await p.mouse.click(point.x,point.y);
  }
  async function create(p, pageUrl) {
    await field(p,'#create [name=source]','chromium'); await field(p,'#create [name=encoder]',process.env.PRTS_TEST_ENCODER || 'software');
    await field(p,'#create [name=url]',pageUrl); await field(p,'#create [name=width]',640); await field(p,'#create [name=height]',480);
    await p.click('#create button');
    await p.waitForFunction(() => document.querySelector('video').videoWidth === 640, {timeout:330000});
  }
  const a=await page(); await create(a,url);
  await color(a,350,240,[210,35,60],'real red page decoded');
  const colors=new Set();
  await until(async () => { colors.add((await pixel(a,480,340))[0] > 120); return colors.size===2; },'real animation changes');
  if (process.env.PRTS_M4_BENCH === '1') {
    const exec = promisify(execFile);
    async function cpuSnapshot() {
      assert.ok(Number.isSafeInteger(daemonPid) && daemonPid > 0);
      const command = `$rootPid=${daemonPid}
$tree=New-Object 'System.Collections.Generic.HashSet[int]'
[void]$tree.Add($rootPid)
$processes=Get-CimInstance Win32_Process
for($n=0;$n -lt 8;$n++){foreach($p in $processes){if($tree.Contains([int]$p.ParentProcessId)){[void]$tree.Add([int]$p.ProcessId)}}}
$browserCpu=0.0
foreach($processId in $tree){if($processId -ne $rootPid){$p=Get-Process -Id $processId -ErrorAction SilentlyContinue;if($p){$browserCpu+=$p.TotalProcessorTime.TotalSeconds}}}
$daemonCpu=(Get-Process -Id $rootPid).TotalProcessorTime.TotalSeconds
@{browser=$browserCpu;daemon=$daemonCpu;time=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()}|ConvertTo-Json -Compress`;
      const {stdout} = await exec('powershell.exe', ['-NoProfile','-Command',command], {windowsHide:true});
      return JSON.parse(stdout);
    }
    for (const [width,height] of [[960,640],[1920,1080]]) {
      await field(a,'#resize [name=width]',width); await field(a,'#resize [name=height]',height); await field(a,'#resize [name=dpr]',1);
      await a.click('#resize button');
      await a.waitForFunction(([w,h]) => document.querySelector('video').videoWidth===w && document.querySelector('video').videoHeight===h, {}, [width,height]);
      await sleep(2500);
      const before = await cpuSnapshot(), samples=[];
      for (let n=0;n<8;n++) { await sleep(1050); samples.push(JSON.parse(await a.$eval('#encoder',e=>e.textContent))); }
      const after = await cpuSnapshot(), wall=(after.time-before.time)/1000;
      const captured=samples.reduce((n,s)=>n+s.frames.encoded,0);
      const fps=samples.reduce((n,s)=>n+s.encoder.fps,0)/samples.length;
      const encodeMs=samples.reduce((n,s)=>n+(s.encoder.encode_ms_avg||0)*s.frames.encoded,0)/captured;
      console.log('M4_REAL_BENCH '+JSON.stringify({mode:process.env.PRTS_FRAME_MODE||'auto',width,height,wall,browser_tree_cpu_percent:(after.browser-before.browser)/wall*100,prtsd_cpu_percent:(after.daemon-before.daemon)/wall*100,fps,encode_ms:encodeMs,frame_backend:samples.at(-1).frames.backend,input_path:samples.at(-1).encoder.input_path}));
    }
    await field(a,'#resize [name=width]',640); await field(a,'#resize [name=height]',480); await a.click('#resize button');
    await a.waitForFunction(() => document.querySelector('video').videoWidth===640 && document.querySelector('video').videoHeight===480);
  }
  await field(a,'#navigate [name=url]',url.replace('127.0.0.1','localhost')+'blue');
  await a.click('#navigate button'); await color(a,350,240,[20,60,210],'cross-site navigation produced new page frames');
  await a.waitForFunction(() => document.querySelector('#tabs button.active').textContent.includes('localhost:fixture'));
  await field(a,'#navigate [name=url]',url); await a.click('#navigate button');
  await color(a,350,240,[210,35,60],'capture retargeted back across sites');
  await remoteClick(a,80,60); await color(a,350,240,[20,190,60],'DataChannel click changed real page');
  await remoteClick(a,100,180); await a.keyboard.type('PRTS-m3');
  await color(a,480,80,[20,60,220],'committed keyboard text changed real page pixels');
  await a.waitForFunction(() => document.querySelector('#tabs button.active').textContent.includes('typed:PRTS-m3'));
  console.log('Decoded page colors, animation, actual click and typed text passed');

  await a.click('#new-tab'); await a.waitForFunction(() => document.querySelectorAll('#tabs button').length===2);
  await field(a,'#navigate [name=url]',url+'blue-static'); await a.click('#navigate button');
  await color(a,350,240,[20,60,210],'second static target decoded');
  await field(a,'#resize [name=width]',480); await field(a,'#resize [name=height]',320); await field(a,'#resize [name=dpr]',2);
  await a.click('#resize button'); await a.waitForFunction(() => document.querySelector('video').videoWidth===960 && document.querySelector('video').videoHeight===640);
  await color(a,700,480,[20,60,210],'resize with DPR2 retained page pixels');
  await remoteClick(a,160,120); await color(a,700,480,[20,190,60],'frontend scaled DPR2 click coordinates');
  await remoteClick(a,200,360); await a.keyboard.type('DPR2');
  await color(a,920,160,[20,60,220],'DPR2 text reached the real page');
  await a.waitForFunction(() => document.querySelector('#tabs button.active').textContent.includes('typed:DPR2'));
  await a.click('#tabs button:first-child'); await a.waitForFunction(() => document.querySelector('video').videoWidth===640);
  await color(a,350,240,[20,190,60],'first target retained page input');
  await a.click('#tabs button:last-child'); await a.waitForFunction(() => document.querySelector('video').videoWidth===960);
  await color(a,700,480,[20,190,60],'static refresh on resubscription');
  await a.click('#close-tab'); await a.waitForFunction(() => document.querySelectorAll('#tabs button').length===1 && document.querySelector('video').videoWidth===640);
  console.log('Two targets, resize/DPR2, switching, static resubscription and close passed');

  const b=await page(); await create(b,url+'blue'); await color(b,350,240,[20,60,210],'second independent instance');
  for (const p of [a,b]) {
    await p.bringToFront();
    await p.waitForFunction(() => /FPS: [1-9]/.test(document.querySelector('#quality').textContent) && document.querySelector('#encoder').textContent.includes('capture_to_encode_ms'));
    const stats = await p.$eval('#encoder',e=>e.textContent);
    console.log(stats);
    const mode = process.env.PRTS_FRAME_MODE || 'auto';
    if (mode === 'cpu') assert.match(stats, /cpu-section/);
    else {
      assert.match(stats, /gpu-texture/);
      const hardware = ['auto','qsv','onevpl','mf'].includes(process.env.PRTS_TEST_ENCODER);
      assert.match(stats, hardware ? /d3d11-zero-copy/ : /staging/);
    }
    if (process.env.PRTS_TEST_ENCODER === 'onevpl') assert.match(stats, /oneVPL/);
    assert.equal(await p.$eval('#error',e=>e.textContent),'');
  }
  await b.bringToFront();
  await b.screenshot({path:fileURLToPath(new URL('../../docs/prts/phase2-m4-e2e.png',import.meta.url)),fullPage:true});
  assert.deepEqual(errors,[]);
  const ids = await b.$$eval('#instances .instance > button:first-child',bs=>bs.map(b=>b.textContent.split(' ')[0]));
  for (const id of ids) {
    await b.$$eval('#instances .instance',(rows,id)=>rows.find(r=>r.firstChild.textContent.startsWith(id+' ')).lastChild.click(),id);
    await until(async () => !(await b.$$eval('#instances .instance > button:first-child',(bs,id)=>bs.some(b=>b.textContent.startsWith(id+' ')),id)),`instance ${id} closed`);
  }
  // Cancel a new instance while its component browser is still starting.
  await b.bringToFront(); await b.click('#create button');
  await until(async () => (await b.$eval('#instances',e=>e.textContent)).includes('starting'),'asynchronous starting instance visible');
  const closing=Date.now();
  await b.$$eval('#instances .instance',rows=>rows[0].lastChild.click());
  await until(async () => (await b.$$('#instances .instance')).length===0,'startup cancellation closes instance',5000);
  assert.ok(Date.now()-closing < 5000);
  await sleep(1500);
  assert.equal((await b.$$('#instances .instance')).length,0,'cancelled browser did not publish late ready');
  assert.deepEqual(errors,[]);
  console.log('Browser frame backend WebRTC E2E passed; instances closed');
});

test('browser launch failure is asynchronous, retains failed ID, and closes cleanly', {timeout:30000}, async t => {
  const server=fork(process.env.PRTS_TEST_SERVER_DIR ? join(process.env.PRTS_TEST_SERVER_DIR,'index.js') : fileURLToPath(new URL('../index.js',import.meta.url)),[],{
    cwd:fileURLToPath(new URL('../../',import.meta.url)),windowsHide:true,silent:true,
    env:{...process.env,PORT:'0',PRTSD_PATH:process.env.PRTSD_PATH || fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe',import.meta.url)),PRTSD_ARGS:'[]',PRTS_CHROME_PATH:'C:/__prts_missing_test_browser__/chrome.exe'},
  });
  const exit=once(server,'exit'); let ws,daemonPid,logs='';
  server.stdout.on('data',d=>{logs+=d;});server.stderr.on('data',d=>{logs+=d;});
  t.after(async()=>{
    ws?.close(); if(server.connected) server.disconnect();
    await deadline(exit,8000,`failure-test server shutdown\n${logs}`);
    if(daemonPid) await until(()=>!alive(daemonPid),'failure-test daemon exit',6000);
    assert.equal(server.exitCode,0,logs);
  });
  const [ready]=await deadline(once(server,'message'),10000,'failure-test server ready');daemonPid=ready.daemonPid;
  ws=new WebSocket(ready.url.replace('http:','ws:')+'/ws',{origin:ready.url});await once(ws,'open');
  const pending=new Map(),events=[];let next=0;
  ws.on('message',bytes=>{const m=JSON.parse(bytes);if(m.id){pending.get(m.id)?.(m);pending.delete(m.id);}else events.push(m);});
  const rpc=(method,params={})=>deadline(new Promise(resolve=>{const id=`failure${++next}`;pending.set(id,resolve);ws.send(JSON.stringify({v:1,id,method,params}));}),6000,method);
  await rpc('hello',{versions:[1]});
  const created=await rpc('instance.create',{options:{source:'chromium',encoder:'software',size:{width:320,height:240}}});
  assert.equal(created.result.instance.state,'starting');
  const id=created.result.instance.instance_id;
  await until(()=>events.some(e=>e.event==='instance.failed'&&e.data.instance_id===id),'asynchronous browser launch failure');
  const list=await rpc('instance.list');assert.equal(list.result.instances.find(i=>i.instance_id===id).state,'failed');
  assert.equal((await rpc('instance.close',{instance_id:id})).result.closed,true);
});
