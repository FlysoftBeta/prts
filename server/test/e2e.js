import test from 'node:test';
import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { once } from 'node:events';
import { access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import puppeteer from 'puppeteer-core';

const root = fileURLToPath(new URL('../../', import.meta.url));
const encoder = process.env.PRTS_TEST_ENCODER || 'software';
assert.ok(['auto', 'qsv', 'software'].includes(encoder), 'Invalid PRTS_TEST_ENCODER');
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

test('real prtsd → Node → browser video, input, multiple viewers, bitrate and packet shaping', { timeout: 240000 }, async t => {
  const executablePath = await browserPath();
  const daemonPath = process.env.PRTSD_PATH || fileURLToPath(new URL('../../prtsd/target/release/prtsd.exe', import.meta.url));
  await access(daemonPath); // Build explicitly before running; never substitute the mock.
  const server = fork(process.env.PRTS_TEST_SERVER_DIR ? join(process.env.PRTS_TEST_SERVER_DIR,'index.js') : fileURLToPath(new URL('../index.js', import.meta.url)), [], {
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
    // Test-only access to actual receiver reports, including cumulative packet loss.
    await p.evaluateOnNewDocument(() => {
      const Peer = window.RTCPeerConnection;
      window.__prtsTestPeers = [];
      window.RTCPeerConnection = class extends Peer {
        constructor(...args) { super(...args); window.__prtsTestPeers.push(this); }
      };
    });
    p.on('pageerror', e => errors.push(e.message));
    await p.goto(ready.url);
    await p.waitForFunction(() => document.querySelector('#status').textContent.includes('connected'));
    return p;
  }
  async function field(p, selector, value) {
    await p.$eval(selector, (e, v) => { e.focus(); e.value = v; }, String(value));
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
  async function receiver(p) {
    return p.evaluate(async () => {
      const pc = window.__prtsTestPeers.findLast(pc => pc.connectionState === 'connected');
      if (!pc) return null;
      const reports = [...(await pc.getStats()).values()];
      const video = reports.find(s => s.type === 'inbound-rtp' && s.kind === 'video');
      if (!video) return null;
      return { packetsLost: video.packetsLost, packetsReceived: video.packetsReceived,
        framesDecoded: video.framesDecoded, jitterMs: video.jitter * 1000,
        width: video.frameWidth, height: video.frameHeight };
    });
  }
  async function network(p, { cap = '', latency = 0, loss = 0 } = {}) {
    await field(p, '#network [name=cap]', cap);
    await field(p, '#network [name=latency]', latency);
    await field(p, '#network [name=loss]', loss);
    // Clear the prior acknowledgement so this observes this request's response.
    await p.$eval('#session-state', e => { e.textContent = ''; });
    await p.click('#network button');
    await p.waitForFunction(() => document.querySelector('#session-state').textContent.includes('settings accepted'));
  }
  async function measure(p, count = 5) {
    const started = Date.now(), firstReceiver = await receiver(p);
    const samples = [];
    let latest;
    for (let i = 0; i < count; i++) { latest = await stats(p); samples.push(latest.encoder); await sleep(1050); }
    const mean = key => {
      const values = samples.map(s => s[key]).filter(Number.isFinite);
      return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
    };
    const lastReceiver = await receiver(p);
    return { name: samples.at(-1).name, bitrate_bps: mean('bitrate_bps'), fps: mean('fps'),
      decoded_fps: (lastReceiver.framesDecoded - firstReceiver.framesDecoded) * 1000 / (Date.now() - started),
      encode_ms_avg: mean('encode_ms_avg'), samples, frames: latest.frames, sessions: latest.sessions,
      receiver: lastReceiver };
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
  await field(a, '#create [name=source]', 'synthetic');
  await field(a, '#create [name=encoder]', encoder);
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
  const clickSamples = [];
  try {
    await until(async () => { const [r, g, b] = await pixels(a, 1010, 590); clickSamples.push([r,g,b]); return r > 160 && g < 100 && b < 100; }, 'decoded red click flash', 3000);
  } catch (error) {
    console.log('Click diagnostics:', JSON.stringify({ clickSamples, receiver: await receiver(a), stats: await stats(a), frontendError: await a.$eval('#error', e => e.textContent) }));
    throw error;
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
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 960 && document.querySelector('video').videoHeight === 540).catch(async error => {
    console.log('Resize diagnostics:', await receiver(a), await stats(a), await a.$eval('#error', e => e.textContent),
      await a.$eval('#resize', f => [...new FormData(f)]), await a.$eval('#session-state', e => e.textContent), logs);
    throw error;
  });
  await a.click('#tabs button:first-child');
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 1280);
  await until(async () => (await pixels(a, 1000, 580)).slice(0, 3).every(n => n > 200), 'original tab retains its pointer');
  await a.click('#tabs button:last-child');
  await a.waitForFunction(() => document.querySelector('video').videoWidth === 960);
  await a.click('#close-tab');
  await a.waitForFunction(() => document.querySelectorAll('#tabs button').length === 1 && document.querySelector('video').videoWidth === 1280);
  console.log('Tab create/switch/resize/close retained the existing WebRTC viewer');

  const b = await page();
  await field(b, '#create [name=source]', 'synthetic');
  await field(b, '#create [name=encoder]', encoder);
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
  const baseline = await measure(b, 4);
  console.log(`Encoder ${encoder}, baseline: ${JSON.stringify(baseline)}`);
  await network(b, { cap: 150 });
  await sleep(2500);
  const low = await measure(b);
  assert.ok(low.fps >= 15, `150 kbps must sustain >=15 encoded FPS: ${JSON.stringify(low)}`);
  assert.equal(low.receiver.width, 1280); assert.equal(low.receiver.height, 720);
  // Congestion feedback may already put the baseline below this ceiling.
  // Allow short-window rate-control/GOP overhead while checking the actual cap.
  assert.ok(low.bitrate_bps <= 150_000 * 1.5, `150 kbps cap exceeded: ${JSON.stringify(low)}`);
  console.log(`Encoder ${encoder}, 150 kbps at unchanged 720p: ${JSON.stringify(low)}`);
  await network(b, { cap: 50 });
  await sleep(2500); // Exclude the partial interval and reconfiguration IDR burst.
  const capped = await measure(b);
  assert.ok(capped.bitrate_bps < baseline.bitrate_bps * 0.8, `50 kbps cap did not reduce bitrate: ${JSON.stringify(capped)}`);
  const cappedFrames = await b.$eval('video', v => v.getVideoPlaybackQuality().totalVideoFrames);
  await playing(b, cappedFrames + 5);
  console.log(`Encoder ${encoder}, 50 kbps: ${JSON.stringify(capped)}`);
  await network(b);
  // Shape the independent first instance so the low-cap receiver's congestion
  // recovery does not starve this test of packets. Its other viewer stays live.
  const shaped = a;
  await shaped.bringToFront();
  await sleep(2500);
  const jitterBaseline = [];
  for (let i = 0; i < 20; i++) { jitterBaseline.push((await receiver(shaped)).jitterMs); await sleep(100); }
  await network(shaped, { latency: 400 });
  const jitterDelayed = [];
  for (let i = 0; i < 70; i++) { jitterDelayed.push((await receiver(shaped)).jitterMs); await sleep(100); }
  const beforeJitter = Math.max(...jitterBaseline), delayedJitter = Math.max(...jitterDelayed);
  assert.ok(delayedJitter > beforeJitter + 3,
    `400 ms outgoing latency must increase transition jitter: ${beforeJitter} -> ${delayedJitter} ms`);
  console.log(`400 ms outgoing latency: receiver peak jitter ${beforeJitter.toFixed(2)} -> ${delayedJitter.toFixed(2)} ms (ICE RTT bypasses media shaping)`);
  await network(shaped);
  await sleep(2000);
  const lossBefore = await receiver(shaped);
  const droppedBefore = (await stats(shaped)).sessions.reduce((sum, s) => sum + s.shaper_dropped_packets, 0);
  await network(shaped, { loss: 15 });
  let lossPeak = lossBefore.packetsLost;
  for (let i = 0; i < 80; i++) {
    lossPeak = Math.max(lossPeak, (await receiver(shaped)).packetsLost);
    await sleep(100);
  }
  const lossAfter = await receiver(shaped);
  const droppedAfter = (await stats(shaped)).sessions.reduce((sum, s) => sum + s.shaper_dropped_packets, 0);
  assert.ok(lossPeak > lossBefore.packetsLost,
    `15% outgoing loss must increase receiver packetsLost: ${JSON.stringify({ lossBefore, lossAfter, lossPeak, droppedBefore, droppedAfter })}`);
  assert.ok(droppedAfter > droppedBefore, `Daemon shaper must count dropped packets: ${droppedBefore} -> ${droppedAfter}`);
  console.log(`15% outgoing loss: packetsLost ${lossBefore.packetsLost} -> peak ${lossPeak} (final ${lossAfter.packetsLost}); daemon drops ${droppedBefore} -> ${droppedAfter}`);
  await network(shaped);
  const restoredFrames = (await receiver(shaped)).framesDecoded;
  await until(async () => (await receiver(shaped)).framesDecoded > restoredFrames + 10, 'video resumes after restoring network');
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
