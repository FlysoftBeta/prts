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
  function aboutLabels() {
    function find(root,selector) {
      const match=root.querySelector(selector);if(match)return match;
      for(const el of root.querySelectorAll('*'))if(el.shadowRoot){const found=find(el.shadowRoot,selector);if(found)return found;}
      return null;
    }
    const about=find(document,'settings-about-page');if(!about)return false;
    const root=about.shadowRoot||about, product=find(root,'.product-title'), section=find(root,'settings-section');
    return product && section ? {product:product.textContent.trim(),heading:section.getAttribute('page-title')} : false;
  }
  await page.waitForFunction(aboutLabels);
  const about=await page.evaluate(aboutLabels);
  assert.equal(about.product,'PRTS');assert.equal(about.heading,'About PRTS');
  console.log('BRANDING '+JSON.stringify({resource,versionLabel:label,about}));
});
