import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { Daemon } from '../daemon.js';

// Expected values belong to PIN (chrome/VERSION), independently of app branding.
const version = '158.0.8111.0', major = '158';
const ua = `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${major}.0.0.0 Safari/537.36`;
// PIN's normal Chrome permutation/GREASE for milestone 158.
const brands = [{brand:'Chromium',version:major},{brand:'Not.A/Brand',version:'24'},{brand:'Google Chrome',version:major}];
const fullBrands = brands.map(b => ({...b,version:b.brand === 'Not.A/Brand' ? '24.0.0.0' : version}));
const hints = ['architecture','bitness','platformVersion','fullVersionList','uaFullVersion','wow64'];
const acceptCH = 'Sec-CH-UA-Full-Version-List, Sec-CH-UA-Platform-Version, Sec-CH-UA-Arch, Sec-CH-UA-Bitness, Sec-CH-UA-Full-Version, Sec-CH-UA-WoW64';
const snapshot = `async function snapshot() {
  return {ua:navigator.userAgent,brands:navigator.userAgentData.brands,
    mobile:navigator.userAgentData.mobile,platform:navigator.userAgentData.platform,
    high:await navigator.userAgentData.getHighEntropyValues(${JSON.stringify(hints)})};
}`;
const report = kind => `${snapshot} snapshot().then(data=>fetch('/report/${kind}',{method:'POST',body:JSON.stringify(data)})).catch(error=>fetch('/report/${kind}',{method:'POST',body:JSON.stringify({error:String(error)})}));`;
const sleep = ms => new Promise(resolve => setTimeout(resolve,ms));

test('production pipe launch: Chrome identity in HTTP, pages, dedicated/shared/service workers', {timeout:360000}, async t => {
  const reports = new Map(), requests = new Map();
  const fixture = createServer(async (req,res) => {
    res.setHeader('Accept-CH',acceptCH);res.setHeader('Cache-Control','no-store');
    const path = req.url.split('?')[0];requests.set(path,req.headers);
    if (path.startsWith('/report/')) {
      let body=''; for await (const chunk of req) body+=chunk;
      try { reports.set(path.slice(8),JSON.parse(body)); } catch { reports.set(path.slice(8),{error:body}); }
      res.end('ok'); return;
    }
    res.setHeader('Content-Type',path.endsWith('.js') ? 'text/javascript' : 'text/html');
    if (path === '/worker.js') { res.end(report('worker')); return; }
    if (path === '/shared.js') { res.end(`onconnect=()=>{${report('shared')}}`); return; }
    if (path === '/sw.js') { res.end(`oninstall=e=>e.waitUntil(skipWaiting());onactivate=e=>e.waitUntil((async()=>{await clients.claim();${snapshot}const data=await snapshot();await fetch('/report/service',{method:'POST',body:JSON.stringify(data)});})());`); return; }
    res.end(`<!doctype html><title>PRTS identity gate</title><script>
      ${snapshot}
      (async()=>{
        const data=await snapshot();
        Object.assign(data,{webdriver:navigator.webdriver,chrome:!!window.chrome,
          plugins:Array.from(navigator.plugins,p=>p.name),mimeTypes:Array.from(navigator.mimeTypes,m=>m.type),
          inner:[innerWidth,innerHeight],outer:[outerWidth,outerHeight],screen:[screen.width,screen.height]});
        await fetch('/report/page',{method:'POST',body:JSON.stringify(data)});
        window.worker=new Worker('/worker.js');window.shared=new SharedWorker('/shared.js');
        await navigator.serviceWorker.register('/sw.js');await navigator.serviceWorker.ready;
      })().catch(error=>fetch('/report/error',{method:'POST',body:JSON.stringify({error:String(error)})}));
    </script>`);
  });
  fixture.listen(0,'127.0.0.1'); await once(fixture,'listening');
  const daemon = new Daemon(process.env.PRTSD_PATH || fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe',import.meta.url)),[],fileURLToPath(new URL('../../',import.meta.url)));
  let startupFailure;
  daemon.on('event',message=>{if(message.event==='instance.failed')startupFailure=message.data;});
  const exit = once(daemon.child,'exit');
  t.after(async()=>{
    daemon.stop();
    await Promise.race([exit,sleep(8000).then(()=>{if(daemon.child.exitCode===null)throw Error('daemon teardown deadline');})]);
    fixture.closeAllConnections();fixture.close();assert.equal(daemon.child.exitCode,0);
  });
  await daemon.ready;
  const {instance} = await daemon.request('instance.create',{options:{source:'chromium',encoder:'software',initial_url:`http://127.0.0.1:${fixture.address().port}/`,size:{width:960,height:640,dpr:1}}});
  // Match the existing browser gates' cold-DLL startup allowance. The pipe
  // handshake deadline and all identity/pixel/input checks are unchanged.
  const deadline=Date.now()+330000;
  while(reports.size<4 && !reports.has('error') && !startupFailure && !daemon.dead && Date.now()<deadline) await sleep(50);
  assert.equal(startupFailure,undefined,JSON.stringify(startupFailure));
  console.log('IDENTITY reports '+JSON.stringify(Object.fromEntries(reports)));
  console.log('IDENTITY request headers '+JSON.stringify(Object.fromEntries(requests)));
  assert.deepEqual([...reports.keys()].sort(),['page','service','shared','worker']);
  const page=reports.get('page');
  for (const [kind,data] of reports) {
    assert.equal(data.error,undefined,kind);assert.equal(data.ua,ua,kind);
    assert.deepEqual(data.brands,brands,kind);assert.equal(data.mobile,false,kind);assert.equal(data.platform,'Windows',kind);
    assert.deepEqual(data.high.fullVersionList,fullBrands,kind);assert.equal(data.high.uaFullVersion,version,kind);
    assert.deepEqual(data.high.brands,brands,kind);assert.equal(data.high.platform,'Windows',kind);assert.equal(data.high.mobile,false,kind);
    assert.equal(data.high.architecture,'x86',kind);assert.equal(data.high.bitness,'64',kind);assert.equal(data.high.wow64,false,kind);
    assert.match(data.high.platformVersion,/^\d+\.\d+\.0$/,kind);assert.equal(data.high.platformVersion,page.high.platformVersion,kind);
    const headers=requests.get(`/report/${kind}`), format=list=>list.map(b=>`"${b.brand}";v="${b.version}"`).join(', ');
    assert.equal(headers['user-agent'],ua,kind);
    if (kind === 'page') {
      assert.equal(headers['sec-ch-ua'],format(brands),kind);
      assert.equal(headers['sec-ch-ua-full-version-list'],format(fullBrands),kind);
      for (const [key,value] of Object.entries({'platform':'Windows','platform-version':data.high.platformVersion,arch:'x86',bitness:'64'})) assert.equal(headers[`sec-ch-ua-${key}`],`"${value}"`,kind);
      assert.equal(headers['sec-ch-ua-mobile'],'?0',kind);
    } else {
      // PIN's WorkerFetchContext does not add Client Hints to fetches (only
      // FrameFetchContext does). Confirmed with the stock Edge reference too.
      // Require the normal absence, rather than silently accepting any values.
      assert.deepEqual(Object.keys(headers).filter(k=>k.startsWith('sec-ch-ua')),[],kind);
    }
  }
  for (const path of ['/','/worker.js','/shared.js','/sw.js']) {
    assert.equal(requests.get(path)['user-agent'],ua,path);
    if(path === '/') assert.equal(requests.get(path)['sec-ch-ua'],brands.map(b=>`"${b.brand}";v="${b.version}"`).join(', '),path);
    else assert.deepEqual(Object.keys(requests.get(path)).filter(k=>k.startsWith('sec-ch-ua')),[],path);
  }
  assert.equal(page.webdriver,false);assert.equal(page.chrome,true);
  assert.deepEqual(page.plugins,['PDF Viewer','Chrome PDF Viewer','Chromium PDF Viewer','Microsoft Edge PDF Viewer','WebKit built-in PDF']);
  assert.deepEqual(page.mimeTypes,['application/pdf','text/pdf']);
  for(let i=0;i<2;i++) { assert.ok(page.inner[i]>0);assert.ok(page.outer[i]>=page.inner[i]);assert.ok(page.screen[i]>=page.outer[i]); }
  const {stdout}=await promisify(execFile)('powershell.exe',['-NoProfile','-Command',`
    $identityTree=New-Object 'System.Collections.Generic.HashSet[int]'
    [void]$identityTree.Add([int]$env:PRTS_IDENTITY_DAEMON_PID)
    $identityProcesses=Get-CimInstance Win32_Process
    for($n=0;$n -lt 8;$n++){foreach($childProcess in $identityProcesses){if($identityTree.Contains([int]$childProcess.ParentProcessId)){[void]$identityTree.Add([int]$childProcess.ProcessId)}}}
    @($identityTree | Where-Object {$_ -ne [int]$env:PRTS_IDENTITY_DAEMON_PID}) | ConvertTo-Json -Compress
  `],{windowsHide:true,timeout:15000,env:{...process.env,PRTS_IDENTITY_DAEMON_PID:String(daemon.child.pid)}});
  const children=JSON.parse(stdout.replace(/^\uFEFF/,''));
  assert.ok(children.length>0,'Expected owned browser processes');
  const alive=pid=>{try{process.kill(pid,0);return true;}catch(error){if(error.code==='ESRCH')return false;throw error;}};
  const closing=Date.now();
  await daemon.request('instance.close',{instance_id:instance.instance_id});
  const closedBy=Date.now()+2000;
  while(children.some(alive) && Date.now()<closedBy) await sleep(20);
  assert.deepEqual(children.filter(alive),[],'Worker fixture browser Job leaked processes');
  console.log('IDENTITY close_ms='+ (Date.now()-closing)+' all owned browser PIDs exited: '+children.join(','));
});
