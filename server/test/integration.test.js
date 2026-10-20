import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { WebSocket } from 'ws';

const root = fileURLToPath(new URL('../../', import.meta.url));
const mock = fileURLToPath(new URL('./mock-prtsd.js', import.meta.url));

test('HTTP and WS control plane with fragmented/coalesced mock stdio', { timeout: 20000 }, async t => {
  const server = spawn(process.execPath, ['server'], { cwd: root, windowsHide: true,
    env: { ...process.env, PORT: '0', PRTS_ISOLATION_PROFILE: 'none', PRTSD_PATH: process.execPath, PRTSD_ARGS: JSON.stringify([mock]) }, stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = ''; server.stderr.on('data', data => { stderr += data; });
  t.after(async () => {
    if (server.exitCode === null) {
      // Windows kill is not a POSIX signal. Closing the fixture sockets first
      // exercises session cleanup; its parent EOF also shuts down the mock.
      const exit = once(server, 'exit'); server.kill(); await exit;
    }
  });
  const url = await new Promise((resolve, reject) => {
    let output = '';
    server.stdout.on('data', data => { output += data; const match = output.match(/http:\/\/127\.0\.0\.1:\d+/); if (match) resolve(match[0]); });
    server.on('exit', code => reject(new Error(`Server exited ${code}: ${stderr}`)));
  });
  const page = await fetch(url); assert.equal(page.status, 200); assert.match(await page.text(), /Remote browser/);
  assert.equal((await fetch(`${url}/..%2fpackage.json`)).status, 404);
  await assert.rejects(new Promise((resolve, reject) => {
    const bad = new WebSocket(url.replace('http:', 'ws:') + '/ws', { origin: 'http://evil.invalid' });
    bad.on('open', () => { bad.terminate(); resolve(); }); bad.on('error', reject);
  }));

  async function client() {
    const ws = new WebSocket(url.replace('http:', 'ws:') + '/ws', { origin: url });
    const responses = new Map(), events = [], order = [], waiters = [];
    let id = 0;
    ws.on('message', bytes => {
      const m = JSON.parse(bytes); order.push(m);
      if (m.id) { const p = responses.get(m.id); responses.delete(m.id); m.error ? p.reject(Object.assign(new Error(m.error.message), m.error)) : p.resolve(m.result); }
      else { events.push(m); for (const waiter of [...waiters]) if (waiter.predicate(m)) { waiters.splice(waiters.indexOf(waiter), 1); waiter.resolve(m); } }
    });
    await once(ws, 'open');
    const rpc = (method, params = {}) => new Promise((resolve, reject) => {
      const key = `c${++id}`; responses.set(key, { resolve, reject }); ws.send(JSON.stringify({ v: 1, id: key, method, params }));
    });
    const event = predicate => {
      const existing = events.find(predicate);
      return existing ? Promise.resolve(existing) : new Promise(resolve => waiters.push({ predicate, resolve }));
    };
    t.after(() => ws.terminate());
    return { ws, rpc, event, order };
  }
  const a = await client(), b = await client();
  assert.equal((await a.rpc('hello', { versions: [1] })).version, 1);
  const { instance } = await a.rpc('instance.create', { options: { source: 'synthetic', initial_url: 'https://example.org/路径', size: { width: 640, height: 360, dpr: 1 } } });
  assert.equal(instance.state, 'starting');
  assert.equal(instance.options.isolation_profile, 'none', 'Node applies its configured default');
  const explicit = await a.rpc('instance.create', { options: { source: 'synthetic', isolation_profile: 'job' } });
  assert.equal(explicit.instance.options.isolation_profile, 'job', 'Explicit instance policy wins');
  await a.event(e => e.event === 'instance.ready' && e.data.instance_id === explicit.instance.instance_id);
  await a.rpc('instance.close', { instance_id: explicit.instance.instance_id });
  const iid = instance.instance_id;
  await a.event(e => e.event === 'instance.ready' && e.data.instance_id === iid);
  const list = await a.rpc('target.list', { instance_id: iid });
  assert.equal(list.targets.length, 1); assert.equal(list.targets[0].url, 'https://example.org/路径');
  const { session_id: sid } = await a.rpc('session.create', { instance_id: iid, viewer_id: 'test', control: true });
  await assert.rejects(b.rpc('session.offer', { session_id: sid }), { code: 'permission_denied' });
  await assert.rejects(b.rpc('target.activate', { instance_id: iid, target_id: '1' }), { code: 'permission_denied' });
  const offer = await a.rpc('session.offer', { session_id: sid }); assert.equal(offer.description.type, 'offer');
  await a.event(e => e.event === 'session.ice' && e.data.candidate === null);
  const offerIndex = a.order.findIndex(e => e.result?.description?.type === 'offer');
  const iceIndex = a.order.findIndex(e => e.event === 'session.ice');
  assert.ok(offerIndex < iceIndex, 'Node buffers early ICE until after offer response');
  await a.rpc('session.answer', { session_id: sid, description: { type: 'answer', sdp: 'stub' } });
  await a.rpc('session.ice', { session_id: sid, candidate: null });
  const second = await a.rpc('target.create', { instance_id: iid, url: 'about:blank' });
  assert.equal(second.target.target_id, '2');
  await a.rpc('target.activate', { instance_id: iid, target_id: '1' });
  await a.rpc('target.navigate', { instance_id: iid, target_id: '1', url: 'https://example.net/' });
  const resized = await a.rpc('target.resize', { instance_id: iid, target_id: '1', size: { width: 800, height: 600, dpr: 1.5 } });
  assert.equal(resized.target.size.dpr, 1.5);
  await a.rpc('target.close', { instance_id: iid, target_id: '2' });
  const settings = { bitrate_cap_bps: 800000, added_latency_ms: 0, loss_percent: 0, seed: 42 };
  assert.deepEqual((await a.rpc('network.set', { instance_id: iid, settings })).settings, settings);
  await assert.rejects(a.rpc('network.set', { instance_id: iid, settings: { ...settings, loss_percent: 2 } }), { code: 'unsupported' });
  const stats = await a.event(e => e.event === 'stats' && e.data.instance_id === iid);
  assert.equal(stats.data.encoder.name, 'mock-no-encoder'); assert.equal(stats.data.sessions[0].session_id, sid);
  const otherStats = await b.event(e => e.event === 'stats' && e.data.instance_id === iid);
  assert.deepEqual(otherStats.data.sessions, []);
  const closed = once(a.ws, 'close'); a.ws.close(); await closed;
  // A later request is serialized behind Node's disconnect cleanup.
  let replacement;
  for (let attempt = 0; attempt < 20; attempt++) {
    try { replacement = await b.rpc('session.create', { instance_id: iid, viewer_id: 'b', control: true }); break; }
    catch (error) { if (error.code !== 'permission_denied') throw error; await new Promise(resolve => setTimeout(resolve, 25)); }
  }
  assert.ok(replacement, 'Disconnect releases control ownership');
  await b.rpc('session.close', { session_id: replacement.session_id });
  await b.rpc('instance.close', { instance_id: iid });
  assert.deepEqual((await b.rpc('instance.list')).instances, []);
  assert.equal(stderr, '');
});
