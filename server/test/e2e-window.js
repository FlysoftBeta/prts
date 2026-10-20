import test from 'node:test';
import assert from 'node:assert/strict';
import {fork} from 'node:child_process';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import puppeteer from 'puppeteer-core';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(action, label, timeout=30000) {
  const end=Date.now()+timeout;
  while(Date.now()<end) { const value=await action(); if(value) return value; await sleep(50); }
  throw Error(`Timed out: ${label}`);
}
test('M5 single window: v2 root frames through prtsd/WebRTC decode full Chrome UI', {timeout:360000}, async t => {
  const fixture=createServer((req,res)=>{res.setHeader('Content-Type','text/html');res.end(`<title>M5 root fixture</title><style>@keyframes pulse{to{opacity:.1}}#motion{position:absolute;left:20px;top:260px;width:20px;height:20px;background:white;animation:pulse .2s infinite alternate}#gl,#clip{position:absolute;top:80px;width:128px;height:128px}#gl{left:40px}#clip{left:220px}</style><body style="margin:0;background:rgb(220,30,60)"><div id=motion></div><canvas id=gl width=128 height=128></canvas><video id=clip muted autoplay playsinline></video><script>
const gl=document.querySelector('#gl').getContext('webgl');if(gl){gl.clearColor(20/255,190/255,60/255,1);gl.clear(gl.COLOR_BUFFER_BIT);}
const source=document.createElement('canvas');source.width=128;source.height=128;const ctx=source.getContext('2d');let n=0;function paint(){ctx.fillStyle=++n%2?'rgb(20,60,210)':'rgb(230,200,20)';ctx.fillRect(0,0,128,128);}paint();document.querySelector('#clip').srcObject=source.captureStream(10);setInterval(paint,100);
</script>`);});
  fixture.listen(0,'127.0.0.1'); await once(fixture,'listening');
  const server=fork(fileURLToPath(new URL('../index.js',import.meta.url)),[],{
    cwd:fileURLToPath(new URL('../../',import.meta.url)),windowsHide:true,silent:true,
    env:{...process.env,PORT:'0',PRTSD_PATH:fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe',import.meta.url)),PRTSD_ARGS:'[]',PRTS_BROWSER_PROTOCOL:'2'},
  });
  let logs='',browser; const exit=once(server,'exit');
  server.stdout.on('data',d=>logs+=d);server.stderr.on('data',d=>logs+=d);
  t.after(async()=>{await browser?.close();fixture.close();if(server.connected)server.disconnect();await Promise.race([exit,sleep(10000).then(()=>{if(server.exitCode===null){server.kill();throw Error(`Shutdown timeout ${logs}`);}})]);assert.equal(server.exitCode,0,logs);console.log(logs);});
  const [ready]=await once(server,'message');
  browser=await puppeteer.launch({executablePath:process.env.PRTS_BROWSER||'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',headless:true,args:['--autoplay-policy=no-user-gesture-required'],defaultViewport:{width:1280,height:1000}});
  const page=await browser.newPage(); await page.goto(ready.url);
  await page.waitForFunction(()=>document.querySelector('#status').textContent.includes('connected'));
  for(const [name,value] of Object.entries({source:'chromium',encoder:process.env.PRTS_TEST_ENCODER||'qsv',url:`http://127.0.0.1:${fixture.address().port}/`,width:960,height:640})) {
    await page.$eval(`#create [name=${name}]`,(e,value)=>e.value=value,String(value));
  }
  await page.click('#create button');
  await page.waitForFunction(()=>document.querySelector('video').videoWidth===960,{timeout:320000});
  const rois=await until(()=>page.$eval('video',v=>{
    if(v.readyState<2)return false;
    const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;
    const ctx=c.getContext('2d');ctx.drawImage(v,0,0);const {data}=ctx.getImageData(0,0,c.width,c.height);
    const red=(x,y)=>{const i=(y*c.width+x)*4;return Math.abs(data[i]-220)<30&&Math.abs(data[i+1]-30)<30&&Math.abs(data[i+2]-60)<30;};
    if(!red(c.width>>1,c.height*3>>2))return false;
    let pageTop=0;
    for(let y=0;y<c.height/2;y++){let count=0;for(let x=c.width/4|0;x<c.width*3/4;x++)if(red(x,y))count++;if(count>c.width*.45){pageTop=y;break;}}
    if(pageTop<50)return false;
    const bands=[];for(const [start,end] of [[0,pageTop/2|0],[pageTop/2|0,pageTop]]){let low=255,high=0;for(let y=start;y<end;y++)for(let x=0;x<c.width;x++){const value=data[(y*c.width+x)*4];low=Math.min(low,value);high=Math.max(high,value);}bands.push({low,high});}
    return {pageTop,bands,width:c.width,height:c.height};
  }),'decoded Chrome controls above red page');
  assert.equal(rois.height,640);assert.ok(rois.bands.every(b=>b.high-b.low>50),JSON.stringify(rois));
  console.log('M5 decoded full Chrome UI/page ROIs '+JSON.stringify(rois));
  const videoColors=new Set();
  await until(async()=>{
    const samples=await page.$eval('video',(v,top)=>{const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;const ctx=c.getContext('2d');ctx.drawImage(v,0,0);return [104,284].map(x=>[...ctx.getImageData(x,top+144,1,1).data].slice(0,3));},rois.pageTop);
    assert.ok(samples[0].every((v,i)=>Math.abs(v-[20,190,60][i])<45),'WebGL pixels missing from decoded root');
    if(samples[1][2]>150)videoColors.add('blue');if(samples[1][0]>150&&samples[1][1]>140)videoColors.add('yellow');return videoColors.size===2;
  },'animated HTML video pixels through root compositor');
  console.log('M5 decoded WebGL and both animated HTML video colors');
  await page.waitForFunction(()=>{try {const s=JSON.parse(document.querySelector('#encoder').textContent);return s.frames.backend && s.encoder.input_path;}catch{return false;}});
  const stats=JSON.parse(await page.$eval('#encoder',e=>e.textContent));
  if(process.env.PRTS_FRAME_MODE==='cpu')assert.match(stats.frames.backend,/cpu-section/);
  else {assert.match(stats.frames.backend,/gpu-texture/);assert.equal(stats.encoder.input_path,'d3d11-zero-copy');}
  assert.equal(await page.$eval('#error',e=>e.textContent),'');
  console.log('M5 actual video backend '+JSON.stringify(stats));
  await page.$eval('#keyboard',e=>{e.focus();e.blur();});
  await sleep(1200);
  const afterBlur=JSON.parse(await page.$eval('#encoder',e=>e.textContent));
  assert.ok(afterBlur.frames.encoded>0,'logical blur paused the selected window stream');
  console.log('M5 logical blur retained window video publication');
  for (const [width,height,dpr] of [[961,641,1.25],[1001,777,1.25],[1203,803,1]]) {
    for (const [name,value] of Object.entries({width,height,dpr})) {
      await page.$eval(`#resize [name=${name}]`,(e,value)=>e.value=value,String(value));
    }
    await page.click('#resize button');
    const pw=Math.ceil(width*dpr), ph=Math.ceil(height*dpr);
    await page.waitForFunction(([w,h])=>{
      const v=document.querySelector('video');return v.videoWidth===w && v.videoHeight===h;
    },{},[pw+(pw%2),ph+(ph%2)]);
    await until(()=>page.$eval('video',(v,[pw,ph])=>{
      if(v.readyState<2)return false;
      const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;
      const ctx=c.getContext('2d');ctx.drawImage(v,0,0);
      return [[pw-2,ph-2],[pw-2,ph*3>>2],[pw>>1,ph-2]].every(([x,y])=>{
        const p=ctx.getImageData(x,y,1,1).data;return [220,30,60].every((n,i)=>Math.abs(p[i]-n)<35);
      });
    },[pw,ph]),`decoded right/bottom edges ${pw}x${ph}`);
    await page.waitForFunction(([pw,ph])=>{
      const v=document.querySelector('video'),s=document.querySelector('#stage');
      const scale=Math.min(s.clientWidth/pw,s.clientHeight/ph);
      return Math.abs(parseFloat(v.style.width)-v.videoWidth*scale)<.1 &&
        Math.abs(parseFloat(v.style.left)-(s.clientWidth-pw*scale)/2)<.1;
    },{},[pw,ph]);
    const crop=await page.$eval('video',(v,[pw,ph])=>{
      const r=v.getBoundingClientRect(),scale=r.width/v.videoWidth;
      return {inside:document.elementFromPoint(r.left+(pw-.25)*scale,r.top+(ph-.25)*scale)===v,
        outside:pw===v.videoWidth || document.elementFromPoint(r.left+(pw+.5)*scale,r.top+ph/2*scale)!==v,
        clip:getComputedStyle(v).clipPath};
    },[pw,ph]);
    assert.ok(crop.inside && crop.outside,JSON.stringify(crop));
    await sleep(1100);
    const oddStats=JSON.parse(await page.$eval('#encoder',e=>e.textContent));
    if(process.env.PRTS_TEST_ENCODER!=='software') {
      assert.match(oddStats.encoder.name,/Quick Sync/);
      assert.equal(oddStats.encoder.input_path,process.env.PRTS_FRAME_MODE==='cpu'?'cpu-nv12':'d3d11-zero-copy');
    }
    console.log(`PASS odd WebRTC ${width}x${height}@${dpr}: true ${pw}x${ph}, ${oddStats.encoder.name}/${oddStats.encoder.input_path}, frontend crop ${JSON.stringify(crop)}`);
  }
  await page.screenshot({path:fileURLToPath(new URL('../../docs/prts/phase2-m5-e2e.png',import.meta.url)),fullPage:true});
});
