import test from 'node:test';
import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { once } from 'node:events';
import { access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer-core';

const root = fileURLToPath(new URL('../../', import.meta.url));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function deadline(promise, ms, description) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(description)), ms);
    })]);
  } finally { clearTimeout(timer); }
}
async function until(action, description, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const value = await action();
    if (value) return value;
    await sleep(50);
  }
  throw new Error(`Timed out: ${description}`);
}
function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (e) { if (e.code === 'ESRCH') return false; throw e; }
}
async function browserPath() {
  for (const candidate of [process.env.PRTS_BROWSER,
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    'C:/src/chromium/src/out/Default/chrome.exe'].filter(Boolean)) {
    try { await access(candidate); return candidate; } catch {}
  }
  throw new Error('Set PRTS_BROWSER to an installed Chrome/Edge executable');
}

test('real prtsd → Node → browser video, input, multiple viewers and bitrate cap', { timeout: 150000 }, async t => {
  const executablePath = await browserPath();
  const daemonPath = fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe', import.meta.url));
  await access(daemonPath); // Build explicitly before running; never substitute the mock.
  const server = fork(fileURLToPath(new URL('../index.js', import.meta.url)), [], {
    cwd: root, windowsHide: true, silent: true,
    env: { ...process.env, PORT: '0', PRTSD_PATH: daemonPath, PRTSD_ARGS: '[]' },
  });
  let logs = '', browser, daemonPid;
  const serverExit = once(server, 'exit');
  server.stdout.on('data', d => { logs += d; });
  server.stderr.on('data', d => { logs += d; });
  t.after(async () => {
    await browser?.close();
    if (server.connected) server.disconnect();
    await deadline(serverExit, 8000, `Server shutdown timed out\n${logs}`).catch(error => { server.kill(); throw error; });
    if (daemonPid) await until(() => !alive(daemonPid), 'prtsd process exit after graceful shutdown', 6000);
    assert.equal(server.exitCode, 0, logs);
    console.log(`Clean shutdown: server and prtsd PID ${daemonPid} exited`);
  });
  const ready = await deadline(Promise.race([
    once(server, 'message').then(([m]) => m),
    serverExit.then(() => { throw new Error(`Server exited before ready\n${logs}`); }),
  ]), 15000, `Server startup timed out\n${logs}`);
  daemonPid = ready.daemonPid;
  browser = await puppeteer.launch({ executablePath, headless: true,
    args: ['--autoplay-policy=no-user-gesture-required'], defaultViewport: { width: 1280, height: 1000 } });
  const errors = [];
  async function page() {
    const p = await browser.newPage();
    p.on('pageerror', e => errors.push(e.message));
    await p.goto(ready.url);
    await p.waitForFunction(() => document.querySelector('#status').textContent.includes('connected'));
    return p;
  }
  async function field(p, selector, value) {
    await p.$eval(selector, (e, v) => { e.value = v; }, String(value));
  }
  async function playing(p, min = 20) {
    await p.waitForFunction(n => {
      const v = document.querySelector('video');
      return !v.paused && v.readyState >= 2 && v.getVideoPlaybackQuality().totalVideoFrames > n;
    }, { timeout: 30000 }, min);
  }
  async function stats(p) {
    return p.$eval('#encoder', e => { try { return JSON.parse(e.textContent); } catch { return null; } });
  }
  async function quality(p) {
    await p.waitForFunction(() => {
      const s = document.querySelector('#quality').textContent;
      return /RTT: [\d.]+ ms/.test(s) && /FPS: [1-9]/.test(s) && /Received: [1-9]/.test(s) &&
        /Packets lost \(interval\): -?\d+/.test(s) && /Jitter: [\d.]+ ms/.test(s) && /Decode: [\d.]+ ms/.test(s) && s.includes('video/H264');
    }, { timeout: 30000 });
  }
  // Read decoded video content, never browser overlays or outgoing input messages.
  async function pixels(p, x, y, w = 1, h = 1) {
    return p.$eval('video', (v, r) => {
      const c = document.createElement('canvas'); c.width = v.videoWidth; c.height = v.videoHeight;
      const ctx = c.getContext('2d'); ctx.drawImage(v, 0, 0);
      return [...ctx.getImageData(...r).data];
    }, [x, y, w, h]);
  }
  async function position(p, x, y) {
    await p.$eval('video', v => v.scrollIntoView({ block: 'center' }));
    return p.$eval('video', (v, [x, y]) => {
      const r = v.getBoundingClientRect(), s = Math.min(r.width / v.videoWidth, r.height / v.videoHeight);
      return { x: r.x + (r.width - v.videoWidth * s) / 2 + x * s,
        y: r.y + (r.height - v.videoHeight * s) / 2 + y * s };
    }, [x, y]);
  }
  const a = await page();
  await a.click('#create button');
  await playing(a);
  await quality(a);
  const initialFrames = await a.$eval('video', v => v.getVideoPlaybackQuality().totalVideoFrames);
  await playing(a, initialFrames + 10);
  const firstId = (await stats(a)).instance_id;
  console.log(`Live H264 video: ${initialFrames}+ decoded frames; receiver UI stats populated`);

  const point = await position(a, 1000, 580);
  await a.mouse.move(point.x, point.y);
  await until(async () => (await pixels(a, 1000, 580)).slice(0, 3).every(n => n > 200), 'decoded white pointer');
  await a.mouse.down();
  try {
    await until(async () => { const [r, g, b] = await pixels(a, 1010, 590); return r > 160 && g < 100 && b < 100; }, 'decoded red click flash', 3000);
  } finally { await a.mouse.up(); }
  const beforeText = await pixels(a, 88, 72, 160, 16);
  await a.keyboard.type('PRTS-e2e'); // Click focused the frontend's hidden text input.
  await until(async () => {
    const after = await pixels(a, 88, 72, 160, 16);
    return after.filter((n, i) => i % 4 !== 3 && n > 160 && beforeText[i] < 100).length > 150;
  }, 'committed text visible in decoded video');
  assert.equal(await a.$eval('#error', e => e.textContent), '');
  console.log('Real mouse move/down/up and keyboard text are visible in decoded video');

  await a.click('#new-tab');
  await a.waitForFunction(() => document.querySelectorAll('#tabs button').length === 2 && document.querySelector('#tabs button.active').textContent.startsWith('2 '));
  await field(a, '#resize [name=width]', 960);
  await field(a, '#resize [name=height]', 540);
  await a.click('#resize button');
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 960 && document.querySelector('video').videoHeight === 540);
  await a.click('#tabs button:first-child');
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 1280);
  await until(async () => (await pixels(a, 1000, 580)).slice(0, 3).every(n => n > 200), 'original tab retains its pointer');
  await a.click('#tabs button:last-child');
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 960);
  await a.click('#close-tab');
  await a.waitForFunction(() => document.querySelectorAll('#tabs button').length === 1 && document.querySelector('video').videoWidth === 1280);
  console.log('Tab create/switch/resize/close retained the existing WebRTC viewer');

  const b = await page();
  await b.click('#create button');
  await playing(b);
  await quality(b);
  const secondId = (await stats(b)).instance_id;
  assert.notEqual(secondId, firstId);
  await a.bringToFront();
  const prior = await a.$eval('video', v => v.getVideoPlaybackQuality().totalVideoFrames);
  await playing(a, prior + 10);
  // A third page proves fanout: a second viewer of the *same* encoder, view-only.
  const c = await page();
  await c.click('#control');
  await c.$$eval('#instances .instance > button:first-child', (buttons, id) => buttons.find(b => b.textContent.startsWith(id + ' ')).click(), firstId);
  await playing(c);
  await quality(c);
  await a.waitForFunction(() => document.querySelectorAll('#instances .instance').length === 2, { polling: 100 });
  console.log(`Two instances (${firstId}, ${secondId}) and three concurrent viewers play`);

  await b.bringToFront();
  const baseline = [];
  for (let i = 0; i < 4; i++) { baseline.push((await stats(b)).encoder.bitrate_bps); await sleep(1050); }
  const before = baseline.reduce((a, b) => a + b) / baseline.length;
  await field(b, '#network [name=cap]', 50);
  await b.click('#network button');
  await b.waitForFunction(() => document.querySelector('#session-state').textContent.includes('settings accepted'));
  await sleep(2500); // Exclude the partial interval and reconfiguration IDR burst.
  const capped = [];
  for (let i = 0; i < 5; i++) { capped.push((await stats(b)).encoder.bitrate_bps); await sleep(1050); }
  const after = capped.reduce((a, b) => a + b) / capped.length;
  assert.ok(after < before * 0.8, `Encoder bitrate did not drop: ${before} → ${after}; ${JSON.stringify(capped)}`);
  const cappedFrames = await b.$eval('video', v => v.getVideoPlaybackQuality().totalVideoFrames);
  await playing(b, cappedFrames + 5);
  console.log(`Measured encoder bitrate cap: ${Math.round(before)} → ${Math.round(after)} bps (50 kbps target)`);
  await b.evaluate(() => window.scrollTo(0, 0));
  await b.screenshot({ path: fileURLToPath(new URL('../../docs/prts/phase1-e2e.png', import.meta.url)), fullPage: true });
  assert.deepEqual(errors, []);
  for (const p of [a, b, c]) assert.equal(await p.$eval('#error', e => e.textContent), '');
  // Explicit instance close exercises teardown while viewers are still attached.
  await a.bringToFront();
  await a.click('#refresh');
  await a.waitForFunction(() => document.querySelectorAll('#instances .instance').length === 2);
  await a.$$eval('#instances .instance', rows => rows.forEach(row => row.lastElementChild.click()));
  await a.waitForFunction(() => document.querySelectorAll('#instances .instance').length === 0, { polling: 100 }).catch(async error => {
    console.log('Close diagnostics:', await a.$eval('#error', e => e.textContent), await a.$eval('#instances', e => e.textContent), logs);
    throw error;
  });
  for (const p of [a, b, c]) {
    await p.waitForFunction(() => document.querySelectorAll('#instances .instance').length === 0, { polling: 100 });
    assert.equal(await p.$eval('#error', e => e.textContent), '');
  }
});
