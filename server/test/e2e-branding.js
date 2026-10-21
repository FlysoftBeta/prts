import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import puppeteer from 'puppeteer-core';

test('PRTS standard branding in executable resources, version and About UI', {timeout:90000}, async t => {
  const executablePath=process.env.PRTS_CHROME_PATH || 'C:/src/chromium/src/out/Default/chrome.exe';
  const {stdout}=await promisify(execFile)('powershell.exe',['-NoProfile','-Command',
    '[Diagnostics.FileVersionInfo]::GetVersionInfo($env:PRTS_CHROME_PATH) | Select-Object ProductName,FileDescription | ConvertTo-Json -Compress'],
    {env:{...process.env,PRTS_CHROME_PATH:executablePath},windowsHide:true,timeout:15000});
  const resource=JSON.parse(stdout.replace(/^\uFEFF/,''));
  assert.equal(resource.ProductName,'PRTS');assert.match(resource.FileDescription,/PRTS/);
  // CDP only reads internal product pages. Web identity uses the separate
  // production pipe-launch gate, without automation/debugging switches.
  const browser=await puppeteer.launch({executablePath,headless:true,args:['--no-first-run','--lang=en-US']});
  t.after(()=>browser.close());
  const page=await browser.newPage();await page.goto('chrome://version');
  const label=await page.$eval('tr .label',e=>e.textContent.trim());
  assert.equal(label,'PRTS');
  await page.goto('chrome://settings/help');
  await page.waitForFunction(()=>{
    function text(root){return [...root.childNodes].map(n=>n.nodeType===Node.TEXT_NODE?n.textContent:n.shadowRoot?text(n.shadowRoot):text(n)).join(' ');}
    return text(document).includes('About PRTS');
  });
  const about=await page.evaluate(()=>{
    function text(root){return [...root.childNodes].map(n=>n.nodeType===Node.TEXT_NODE?n.textContent:n.shadowRoot?text(n.shadowRoot):text(n)).join(' ');}
    return text(document.querySelector('settings-ui'));
  });
  assert.match(about,/About PRTS/);assert.match(about,/PRTS/);
  console.log('BRANDING '+JSON.stringify({resource,versionLabel:label,about:true}));
});
