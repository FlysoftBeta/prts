// Real public-page WebRTC comparison; requires no AppContainer loopback exemption.
import test from 'node:test';
import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer-core';

test('I1 public page: job and AppContainer decode the same whole-window page through QSV/WebRTC', { timeout: 180000 }, async t => {
  const root=fileURLToPath(new URL('../../',import.meta.url));
  const server=fork(fileURLToPath(new URL('../index.js',import.meta.url)),[],{cwd:root,windowsHide:true,silent:true,
    env:{...process.env,PORT:'0',PRTSD_PATH:fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe',import.meta.url)),PRTSD_ARGS:'[]',PRTS_BROWSER_PROTOCOL:'2'}});
  let browser,logs='';
  const exit=once(server,'exit');
  server.stdout.on('data',data=>logs+=data);server.stderr.on('data',data=>logs+=data);
  t.after(async()=>{
    await browser?.close();if(server.connected)server.disconnect();
    await Promise.race([exit,delay(10000).then(()=>{if(server.exitCode===null){server.kill();throw Error('server shutdown timeout');}})]);
    assert.equal(server.exitCode,0,logs);console.log(logs);
  });
  const [ready]=await once(server,'message');
  browser=await puppeteer.launch({executablePath:process.env.PRTS_BROWSER||'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',headless:true,
    args:['--autoplay-policy=no-user-gesture-required'],defaultViewport:{width:1280,height:1000}});
  const samples=[];
  for(const profile of ['job','appcontainer']) {
    const page=await browser.newPage();await page.goto(ready.url);
    await page.waitForFunction(()=>document.querySelector('#status').textContent.includes('connected'));
    for(const [name,value] of Object.entries({source:'chromium',encoder:'qsv',isolation:profile,url:'https://example.org/',width:960,height:640})) {
      await page.$eval(`#create [name=${name}]`,(e,value)=>e.value=value,String(value));
    }
    await page.click('#create button');
    await page.waitForFunction(()=>document.querySelector('#tabs').textContent.includes('Example Domain'),{timeout:60000});
    await page.waitForFunction(()=>document.querySelector('video').videoWidth===960 && document.querySelector('video').readyState>=2,{timeout:60000});
    await delay(1500);
    const stats=JSON.parse(await page.$eval('#encoder',e=>e.textContent));
    assert.match(stats.encoder.name,/Quick Sync/);assert.equal(stats.encoder.input_path,'d3d11-zero-copy');assert.equal(stats.frames.backend,'gpu-texture');
    samples.push(await page.$eval('video',v=>{
      const c=document.createElement('canvas');c.width=v.videoWidth;c.height=v.videoHeight;const ctx=c.getContext('2d');ctx.drawImage(v,0,0);
      const data=ctx.getImageData(0,0,c.width,c.height).data,points=[];
      for(let y=150;y<620;y+=4)for(let x=20;x<940;x+=4){const i=(y*c.width+x)*4;points.push(data[i],data[i+1],data[i+2]);}
      return points;
    }));
    console.log(`I1 ${profile} public Example Domain metadata and decoded GPU/QSV/WebRTC pixels captured`);
    await page.click('#instances .instance:last-child button:last-child');
    await page.waitForFunction(()=>document.querySelectorAll('#instances .instance').length===0);
    await page.close();
  }
  assert.equal(samples[0].length,samples[1].length);
  const error=samples[0].reduce((sum,value,i)=>sum+Math.abs(value-samples[1][i]),0)/samples[0].length;
  assert(error<12,`public-page decoded pixel MAE ${error}`);
  console.log(`I1 PASS public whole-window decoded job/AppContainer pixel MAE=${error.toFixed(3)}`);
});
