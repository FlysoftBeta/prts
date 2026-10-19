// Protocol fixture only: no encoding, media or real network simulation.
import { Decoder, encode } from '../framing.js';
import { performance } from 'node:perf_hooks';

const started = performance.now();
const instances = new Map(), targets = new Map(), sessions = new Map();
let nextInstance = 0, nextSession = 0, seq = 0, hello = false, interval;
const seen = new Set();
// Intentionally split prefixes and coalesce records. Queue preserves stdout order.
let queue = [], scheduled = false;
function send(message) {
  queue.push(encode(message));
  if (scheduled) return;
  scheduled = true;
  setImmediate(() => {
    const bytes = Buffer.concat(queue); queue = []; scheduled = false;
    process.stdout.write(bytes.subarray(0, 2));
    process.stdout.write(bytes.subarray(2, 7));
    process.stdout.write(bytes.subarray(7));
  });
}
const event = (event, data) => send({ v: 1, event, seq: ++seq, time_ms: Math.floor(performance.now() - started), data });
const reply = (id, result) => send({ v: 1, id, result });
const fail = (code, message) => { throw Object.assign(new Error(message), { code }); };
function instance(id) { return instances.get(id) || fail('not_found', 'Unknown instance'); }
function target(params) { instance(params.instance_id); return targets.get(params.instance_id).get(params.target_id) || fail('not_found', 'Unknown target'); }
function targetEvent(name, i, t) { event(name, { instance_id: i.instance_id, target: t }); }
function activate(i, active) {
  for (const t of targets.get(i.instance_id).values()) {
    if (t.active && t !== active) { t.active = false; targetEvent('target.changed', i, t); }
  }
  active.active = true; active.generation = String(BigInt(active.generation) + 1n);
  i.active_target_id = active.target_id;
  targetEvent('target.changed', i, active);
}
function createTarget(i, url, size, active) {
  const list = targets.get(i.instance_id);
  const t = { target_id: String(++i.nextTarget), opener_target_id: null, kind: 'tab', load: 'loading',
    size, url, title: `PRTS mock: ${url}`, generation: '1', active: list.size === 0 };
  list.set(t.target_id, t);
  if (t.active) i.active_target_id = t.target_id;
  targetEvent('target.created', i, t);
  if (active && !t.active) activate(i, t);
  setImmediate(() => { t.load = 'complete'; if (list.has(t.target_id)) targetEvent('target.changed', i, t); });
  return t;
}
function closeSession(id) {
  const session = sessions.get(id) || fail('not_found', 'Unknown session');
  event('session.closed', { ...session, reason: 'closed' }); sessions.delete(id);
}
function configure(ms) {
  clearInterval(interval);
  if (!ms) return;
  interval = setInterval(() => {
    for (const i of instances.values()) if (i.state === 'ready') event('stats', {
      instance_id: i.instance_id, interval_ms: ms,
      encoder: { name: 'mock-no-encoder', fps: 0, bitrate_bps: 0, qp_avg: null },
      frames: { captured: 0, encoded: 0, dropped: 0, capture_to_encode_ms_avg: null, capture_to_encode_ms_p95: null, capture_to_send_ms_avg: null },
      sessions: [...sessions.values()].filter(s => s.instance_id === i.instance_id).map(s => ({ session_id: s.session_id, rtt_ms: null, available_outgoing_bps: null, rtp_sent_bps: 0, shaper_dropped_packets: 0, viewer_feedback: null })),
    });
  }, ms);
}
function handle({ id, method, params: p }) {
  if (seen.has(id) || (!hello && method !== 'hello')) throw new Error('Invalid request order');
  seen.add(id);
  try {
    if (method === 'hello') {
      if (!p.versions.includes(1)) fail('unsupported', 'Version mismatch');
      hello = true;
      reply(id, { version: 1, daemon: 'prtsd', capabilities: { sources: ['synthetic'], encoders: ['software'], isolation_profiles: ['job'], max_instances: 8, max_width: 8192, max_height: 8192, max_pixels: 64000000 } });
    } else if (method === 'instance.create') {
      const options = { source: 'synthetic', initial_url: 'about:blank', size: { width: 1280, height: 720, dpr: 1 }, encoder: 'software', ...p.options };
      const i = { instance_id: `i${++nextInstance}`, source: options.source, state: 'starting', active_target_id: null, options, nextTarget: 0 };
      instances.set(i.instance_id, i); targets.set(i.instance_id, new Map()); reply(id, { instance: i });
      setImmediate(() => {
        if (!instances.has(i.instance_id)) return;
        createTarget(i, options.initial_url, options.size, true); i.state = 'ready';
        event('instance.ready', { instance_id: i.instance_id, source: i.source, active_target_id: i.active_target_id, encoder: 'software', isolation_profile: 'job' });
      });
    } else if (method === 'instance.list') reply(id, { instances: [...instances.values()] });
    else if (method === 'instance.close') {
      const i = instance(p.instance_id);
      for (const s of [...sessions.values()]) if (s.instance_id === i.instance_id) closeSession(s.session_id);
      for (const t of targets.get(i.instance_id).values()) event('target.closed', { instance_id: i.instance_id, target_id: t.target_id, reason: 'instance closed' });
      targets.delete(i.instance_id); instances.delete(i.instance_id);
      event('instance.closed', { instance_id: i.instance_id, reason: 'closed' }); reply(id, { closed: true });
    } else if (method === 'target.list') { instance(p.instance_id); reply(id, { targets: [...targets.get(p.instance_id).values()] }); }
    else if (method === 'target.create') {
      const i = instance(p.instance_id); reply(id, { target: createTarget(i, p.url || 'about:blank', p.size || i.options.size, p.activate !== false) });
    } else if (method === 'target.activate') { const i = instance(p.instance_id), t = target(p); activate(i, t); reply(id, { target: t }); }
    else if (method === 'target.resize') {
      const i = instance(p.instance_id), t = target(p); t.size = p.size; t.generation = String(BigInt(t.generation) + 1n);
      targetEvent('target.changed', i, t); reply(id, { target: t });
    } else if (method === 'target.navigate') {
      const i = instance(p.instance_id), t = target(p); t.url = p.url; t.title = `PRTS mock: ${p.url}`; t.load = 'loading';
      targetEvent('target.changed', i, t); reply(id, { accepted: true });
      setImmediate(() => { t.load = 'complete'; targetEvent('target.changed', i, t); });
    } else if (method === 'target.close') {
      const i = instance(p.instance_id), t = target(p), list = targets.get(i.instance_id); list.delete(t.target_id);
      event('target.closed', { instance_id: i.instance_id, target_id: t.target_id, reason: 'closed' });
      if (t.active) { i.active_target_id = null; if (list.size) activate(i, list.values().next().value); }
      reply(id, { closed: true });
    } else if (method === 'session.create') {
      instance(p.instance_id);
      if (p.control && [...sessions.values()].some(s => s.instance_id === p.instance_id && s.control)) fail('permission_denied', 'Control occupied');
      const s = { instance_id: p.instance_id, session_id: `s${++nextSession}`, control: !!p.control };
      sessions.set(s.session_id, s); reply(id, { session_id: s.session_id, polite: false, ice_servers: [], control: s.control });
    } else if (method === 'session.close') { closeSession(p.session_id); reply(id, { closed: true }); }
    else if (method === 'session.offer') {
      const s = sessions.get(p.session_id) || fail('not_found', 'Unknown session');
      event('session.ice', { instance_id: s.instance_id, session_id: s.session_id, candidate: { candidate: 'candidate:stub', sdpMid: '0', sdpMLineIndex: 0, usernameFragment: 'mock' } });
      reply(id, { description: { type: 'offer', sdp: 'v=0\r\na=ice-ufrag:mock\r\n' } });
      event('session.ice', { instance_id: s.instance_id, session_id: s.session_id, candidate: null });
    } else if (method === 'session.answer' || method === 'session.ice') reply(id, { accepted: true });
    else if (method === 'network.set') {
      instance(p.instance_id);
      if (p.settings.added_latency_ms || p.settings.loss_percent) fail('unsupported', 'Mock has no packet shaper');
      reply(id, { settings: p.settings });
    } else if (method === 'stats.configure') { configure(p.interval_ms); reply(id, { interval_ms: p.interval_ms }); }
    else fail('unsupported', 'Unknown method');
  } catch (error) { send({ v: 1, id, error: { code: error.code || 'internal', message: error.message, retryable: false } }); }
}
const decoder = new Decoder(handle);
process.stdin.on('data', data => { try { decoder.push(data); } catch (error) { console.error(error.message); process.exit(1); } });
process.stdin.on('end', () => { clearInterval(interval); });
process.stdout.on('error', () => process.exit(1));
