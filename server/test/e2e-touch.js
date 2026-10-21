import test from 'node:test';
import assert from 'node:assert/strict';
import {fork} from 'node:child_process';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
import puppeteer from 'puppeteer-core';

const sleep = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, label, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const v = await fn(); if (v) return v; await sleep(50); }
  throw Error(`Timed out: ${label}`);
}

test('real frontend multi-touch reaches Aura, page events and native gestures', {timeout: 360000}, async t => {
  const dpr = Number(process.env.PRTS_TOUCH_DPR || 1);
  let state = {};
  const fixture = createServer(async (req, res) => {
    if (req.url === '/report') {
      let body = ''; for await (const chunk of req) body += chunk;
      state = JSON.parse(body); res.end('ok'); return;
    }
    res.setHeader('Content-Type', 'text/html');
    res.end(`<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=5"><title>PRTS touch acceptance</title>
      <style>body{margin:0;background:rgb(220,30,60);height:4000px}button{position:absolute;left:40px;top:40px;width:180px;height:100px}#pad{position:absolute;top:180px;width:100%;height:140px;touch-action:none;background:rgb(20,100,210)}</style>
      <button id=button>Tap</button><div id=pad>multi-touch pad</div><script>
      const s={events:[],clicks:0,contexts:0,maxTouches:0,scroll:0,scale:1};
      button.onclick=()=>s.clicks++;
      for(const type of ['pointerdown','pointermove','pointerup','pointercancel','touchstart','touchmove','touchend','touchcancel'])
        document.addEventListener(type,e=>{s.events.push({type,pointerType:e.pointerType,id:e.pointerId,trusted:e.isTrusted,touches:e.touches?.length,scroll:scrollY});s.events=s.events.slice(-200);s.maxTouches=Math.max(s.maxTouches,e.touches?.length||0);},{passive:true});
      document.addEventListener('contextmenu',()=>s.contexts++);
      setInterval(()=>{s.scroll=scrollY;s.scale=visualViewport.scale;fetch('/report',{method:'POST',body:JSON.stringify(s)}).catch(()=>{});},50);
      </script>`);
  });
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening');
  const server = fork(process.env.PRTS_TEST_SERVER_DIR ? join(process.env.PRTS_TEST_SERVER_DIR,'index.js') : fileURLToPath(new URL('../index.js', import.meta.url)), [], {
    windowsHide: true, silent: true,
    env: {...process.env, PORT:'0', PRTSD_PATH:process.env.PRTSD_PATH || fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe', import.meta.url)), PRTSD_ARGS:'[]', PRTS_BROWSER_PROTOCOL:'2'},
  });
  let logs = '', browser, page; const exit = once(server, 'exit');
  server.stdout.on('data', d => logs += d); server.stderr.on('data', d => logs += d);
  t.after(async () => {
    console.log('TOUCH final state '+JSON.stringify(state));
    if (page) console.log('TOUCH frontend '+JSON.stringify(await page.evaluate(()=>({error:document.querySelector('#error').textContent,records:window.touchWire?.length})).catch(()=>null)));
    await browser?.close(); fixture.close();
    if (server.connected) server.disconnect();
    await Promise.race([exit, sleep(10000).then(() => { if (server.exitCode === null) { server.kill(); throw Error(`Shutdown timeout ${logs}`); } })]);
    assert.equal(server.exitCode, 0, logs); console.log(logs);
  });
  const [ready] = await once(server, 'message');
  browser = await puppeteer.launch({executablePath:process.env.PRTS_BROWSER || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',headless:true,args:['--autoplay-policy=no-user-gesture-required'],defaultViewport:{width:1280,height:1000,hasTouch:true}});
  page = await browser.newPage();
  await page.evaluateOnNewDocument(()=>{
    window.touchWire=[];
    const send=RTCDataChannel.prototype.send;
    RTCDataChannel.prototype.send=function(data){if(this.label==='prts.control.v1')window.touchWire.push(JSON.parse(data));return send.call(this,data);};
    const set=RTCPeerConnection.prototype.setRemoteDescription;
    RTCPeerConnection.prototype.setRemoteDescription=function(...args){
      this.addEventListener('datachannel',e=>{if(e.channel.label==='prts.control.v1')window.touchChannel=e.channel;});
      return set.apply(this,args);
    };
  });
  await page.goto(ready.url);
  await page.waitForFunction(() => document.querySelector('#status').textContent.includes('connected'));
  for (const [name,value] of Object.entries({source:'chromium',encoder:process.env.PRTS_TEST_ENCODER || 'qsv',url:`http://127.0.0.1:${fixture.address().port}/`,width:960,height:640,dpr})) {
    await page.$eval(`#create [name=${name}]`,(e,v)=>e.value=v,String(value));
  }
  await page.click('#create button');
  await page.waitForFunction(w=>document.querySelector('video').videoWidth===w,{timeout:320000},960*dpr);
  await until(()=>page.$eval('#encoder',e=>{try{return JSON.parse(e.textContent).encoder.input_path;}catch{return false;}}),'encoder path');
  const stats=JSON.parse(await page.$eval('#encoder',e=>e.textContent));
  if(process.env.PRTS_FRAME_MODE==='cpu')assert.match(stats.frames.backend,/cpu-section/);
  else {
    assert.match(stats.frames.backend,/gpu-texture/);
    assert.equal(stats.encoder.input_path,'d3d11-zero-copy');
    assert.match(stats.encoder.name,/Quick Sync/);
  }
  const top = await until(() => page.$eval('video', (v,dpr) => {
    if(v.readyState<2)return false;
    const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;
    const ctx=c.getContext('2d');ctx.drawImage(v,0,0);
    for(let y=30*dpr;y<250*dpr;y++){const p=ctx.getImageData(700*dpr,y,1,1).data;if(Math.abs(p[0]-220)<25&&Math.abs(p[1]-30)<25&&Math.abs(p[2]-60)<25)return y/dpr;}
    return false;
  }, dpr), 'decoded remote page');
  const rect = await page.$eval('video', v => { const r=v.getBoundingClientRect();return {x:r.left,y:r.top,s:r.width/960}; });
  const cdp = await page.createCDPSession();
  const point = (id,x,y) => ({id,x:rect.x+x*rect.s,y:rect.y+y*rect.s,radiusX:2,radiusY:2,force:.5});
  const send = (type,points) => cdp.send('Input.dispatchTouchEvent',{type,touchPoints:points});
  const tap = async (x,y) => { await send('touchStart',[point(1,x,y)]); await sleep(70); await send('touchEnd',[]); };
  await tap(938,60);
  await until(()=>page.$eval('video',(v,dpr)=>{const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;const ctx=c.getContext('2d');ctx.drawImage(v,0,0);return [...ctx.getImageData(800*dpr,200*dpr,1,1).data].slice(0,3).every(n=>n>200);},dpr),'decoded touch app-menu flyout');
  await tap(670,90);
  // The control-plane targets represent windows; a second Chrome tab keeps
  // the same target ID. Its active URL and decoded page must both change.
  await until(()=>page.$eval('#navigate [name=url]',e=>e.value.startsWith('chrome://newtab')),'touch New tab in app-menu flyout');
  await until(()=>page.$eval('video',(v,dpr)=>{const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;const ctx=c.getContext('2d');ctx.drawImage(v,0,0);return [...ctx.getImageData(800*dpr,400*dpr,1,1).data].slice(0,3).every(n=>n>230);},dpr), 'decoded new-tab page after touching flyout');
  await page.$eval('#navigate [name=url]',(e,url)=>e.value=url,`http://127.0.0.1:${fixture.address().port}/`);
  await page.click('#navigate button');
  await sleep(500);
  await until(()=>state.events.length===0,'fresh fixture after touch New tab');
  await tap(120,top+80);
  await until(()=>state.clicks===1,'native touch tap click');
  assert.ok(state.events.some(e=>e.type==='pointerdown'&&e.pointerType==='touch'&&e.trusted),JSON.stringify(state));
  assert.ok(state.events.some(e=>e.type==='touchstart'&&e.trusted),JSON.stringify(state));
  assert.ok(!state.events.some(e=>e.pointerType==='mouse'),'frontend emulated mouse for touch');
  await send('touchStart',[point(10,180,top+230),point(11,360,top+230)]);
  await until(()=>state.maxTouches===2,'two simultaneous real touch contacts');
  await send('touchMove',[point(10,200,top+240),point(11,380,top+240)]);
  await send('touchEnd',[]);
  await until(()=>state.events.filter(e=>e.type==='pointerup'&&e.pointerType==='touch').length>=3 &&
    state.events.filter(e=>e.type==='touchend').at(-1)?.touches===0,'multi-touch release');
  // Cancellation must reach the page and allow reusing the bounded contact IDs.
  await send('touchStart',[point(12,200,top+230)]);
  await send('touchCancel',[]);
  await until(()=>state.events.some(e=>e.type==='touchcancel'),'contact cancellation');
  await tap(120,top+80); await until(()=>state.clicks===2,'tap after contact cancellation');
  const cancelledBefore = state.events.filter(e=>e.type==='touchcancel').length;
  await send('touchStart',[point(13,200,top+230)]);
  await until(()=>state.events.at(-1)?.type==='touchstart','held touch before channel close');
  await page.evaluate(()=>window.touchChannel.close());
  await until(()=>state.events.filter(e=>e.type==='touchcancel').length>cancelledBefore,'channel-close cancellation of held contact');
  await send('touchCancel',[]);
  await page.click('#connect');
  await until(()=>page.evaluate(()=>window.touchChannel?.readyState==='open' &&
    document.querySelector('#empty').hidden && document.querySelector('video').srcObject),'reconnected touch viewer');
  await send('touchStart',[point(20,700,top+410)]);
  for(let i=1;i<=12;i++){await send('touchMove',[point(20,700,top+410-i*20)]);await sleep(20);}
  await send('touchEnd',[]);
  await until(()=>state.scroll>100,'native one-finger scroll');
  await until(()=>state.events.at(-1)?.type==='touchend','scroll contact release');
  const releasedScroll = state.events.at(-1).scroll;
  await until(()=>state.scroll>releasedScroll+10,'native fling continues scrolling after release');
  await sleep(800);
  const beforeScale=state.scale;
  await send('touchStart',[point(30,350,top+220),point(31,550,top+220)]);
  for(let i=1;i<=12;i++){await send('touchMove',[point(30,350-i*8,top+220),point(31,550+i*8,top+220)]);await sleep(25);}
  await send('touchEnd',[]);
  await until(()=>state.scale>beforeScale*1.1,'native pinch zoom');
  await send('touchStart',[point(40,700,top+360)]); await sleep(1000);
  await send('touchEnd',[]);
  await until(()=>state.contexts>0,'native long-press context menu');
  await tap(800,top+400);
  assert.equal(await page.$eval('#error',e=>e.textContent),'');
  assert.equal(await page.evaluate(()=>visualViewport.scale),1,'video gestures must not zoom the local viewer');
  const wire = await page.evaluate(()=>window.touchWire);
  assert.ok(!wire.some(e=>e.type.startsWith('mouse.')), 'touch must never use mouse wire input');
  console.log('TOUCH gestures PASS '+JSON.stringify({dpr,maxTouches:state.maxTouches,scroll:state.scroll,scale:state.scale,contexts:state.contexts}));
});
