import { RemoteInput } from './input.js';
import { ReceiverStats } from './stats.js';

const $ = id => document.getElementById(id);
const instances = new Map(), targets = new Map(), pending = new Map();
let selected = null, nextId = 0, viewer = null, connecting = false;
const video = $('video');
const report = error => { $('error').textContent = error?.message || String(error); };
const input = new RemoteInput(video, $('keyboard'), $('cursor'), report);
const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws`);

function rpc(method, params = {}) {
  if (ws.readyState !== WebSocket.OPEN) return Promise.reject(new Error('Control plane is disconnected; reload to reconnect'));
  const id = `w${++nextId}`;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timed out`)); }, 12000);
    pending.set(id, { resolve, reject, timer });
    ws.send(JSON.stringify({ v: 1, id, method, params }));
  });
}
const run = action => (...args) => { $('error').textContent = ''; Promise.resolve().then(() => action(...args)).catch(report); };
const sizeFrom = form => ({ width: Number(form.elements.width.value), height: Number(form.elements.height.value), dpr: Number(form.elements.dpr.value) });
function requireInstance() {
  if (!selected) throw new Error('Select an instance first');
  return selected;
}
function activeTarget() { return [...targets.values()].find(t => t.active); }
function requireTarget() {
  const target = activeTarget();
  if (!target) throw new Error('No active tab');
  return { instance_id: requireInstance(), target_id: target.target_id };
}

function renderInstances() {
  $('instances').replaceChildren();
  for (const instance of instances.values()) {
    const row = document.createElement('div'); row.className = 'instance';
    const select = document.createElement('button');
    select.textContent = `${instance.instance_id} · ${instance.source} · ${instance.state}`;
    select.classList.toggle('active', selected === instance.instance_id);
    select.onclick = run(() => selectInstance(instance.instance_id));
    const close = document.createElement('button'); close.textContent = 'Close';
    close.onclick = run(async () => {
      await rpc('instance.close', { instance_id: instance.instance_id });
      instances.delete(instance.instance_id); renderInstances();
    });
    row.append(select, close); $('instances').append(row);
  }
  $('selected').textContent = selected ? `Instance ${selected}` : 'Select an instance';
}
function renderTargets() {
  $('tabs').replaceChildren();
  for (const target of targets.values()) {
    const button = document.createElement('button');
    button.textContent = `${target.target_id} · ${target.title || target.url} (${target.load})`;
    button.title = target.url;
    button.classList.toggle('active', target.active);
    button.onclick = run(async () => {
      await rpc('target.activate', { instance_id: requireInstance(), target_id: target.target_id });
      await refreshTargets();
    });
    $('tabs').append(button);
  }
  const active = activeTarget();
  if (active && document.activeElement !== $('navigate').elements.url) $('navigate').elements.url.value = active.url;
  if (active && !$('resize').contains(document.activeElement)) {
    for (const key of ['width', 'height', 'dpr']) $('resize').elements[key].value = active.size[key] ?? 1;
  }
}
async function refreshInstances() {
  const result = await rpc('instance.list');
  instances.clear();
  for (const instance of result.instances) instances.set(instance.instance_id, instance);
  renderInstances();
}
async function refreshTargets() {
  const instance = selected;
  if (!instance || instances.get(instance)?.state !== 'ready') return;
  const result = await rpc('target.list', { instance_id: instance });
  if (selected !== instance) return;
  targets.clear(); for (const target of result.targets) targets.set(target.target_id, target);
  renderTargets();
}
async function selectInstance(id) {
  await disconnect();
  selected = id;
  targets.clear();
  $('encoder').textContent = 'No server samples yet';
  renderInstances(); renderTargets();
  await refreshTargets();
  if (instances.get(id)?.state === 'ready') await connect();
}
function showTarget(target) {
  input.setTarget(target);
  video.classList.toggle('no-frame', !target);
  $('empty').hidden = !!target;
  if (!target) $('empty').textContent = 'No active remote tab';
}
showTarget(null);

async function disconnect() {
  const old = viewer;
  viewer = null;
  input.release(); input.channel = null; input.control = false; input.sequence = 0;
  showTarget(null);
  video.srcObject = null;
  $('quality').textContent = 'No receiver samples yet';
  $('session-state').textContent = 'No viewer connected';
  if (!old) return;
  old.closed = true;
  old.stats?.stop(); old.pc?.close();
  if (old.id) await rpc('session.close', { session_id: old.id }).catch(() => {});
}
async function connect() {
  if (connecting) return;
  const instance = requireInstance();
  if (instances.get(instance)?.state !== 'ready') throw new Error('Instance is not ready');
  connecting = true;
  try {
    await disconnect();
    const state = { instance, id: null, remoteIce: [], remoteReady: false, closed: false, iceChain: Promise.resolve() };
    viewer = state;
    const session = await rpc('session.create', { instance_id: instance, viewer_id: 'browser', control: $('control').checked });
    state.id = session.session_id;
    if (state.closed || viewer !== state) { await rpc('session.close', { session_id: state.id }); return; }
    const pc = state.pc = new RTCPeerConnection({ iceServers: session.ice_servers });
    input.control = session.control;
    pc.ontrack = e => {
      if (viewer !== state) return;
      video.srcObject = e.streams[0] || new MediaStream([e.track]);
      video.play().catch(report);
      for (const receiver of pc.getReceivers()) {
        try { if ('playoutDelayHint' in receiver) receiver.playoutDelayHint = 0; } catch {}
        try { if ('jitterBufferTarget' in receiver) receiver.jitterBufferTarget = 0; } catch {}
      }
    };
    pc.onconnectionstatechange = () => {
      if (viewer !== state) return;
      $('session-state').textContent = `WebRTC ${pc.connectionState} · ${session.control ? 'control owner' : 'view only'}`;
      if (pc.connectionState === 'failed') report('WebRTC failed. Disconnect and connect again to retry.');
    };
    pc.onicecandidate = e => {
      if (viewer !== state) return;
      const candidate = e.candidate?.toJSON() || null;
      if (candidate && !candidate.usernameFragment) {
        candidate.usernameFragment = pc.localDescription?.sdp.match(/^a=ice-ufrag:(.+)$/m)?.[1].trim();
      }
      rpc('session.ice', { session_id: state.id, candidate }).catch(report);
    };
    pc.ondatachannel = e => {
      const channel = e.channel;
      if (channel.protocol !== 'prts-json-v1') { channel.close(); return; }
      if (channel.label === 'prts.feedback.v1') { state.feedback = channel; return; }
      if (channel.label !== 'prts.control.v1') { channel.close(); return; }
      input.channel = channel;
      channel.onclose = () => {
        if (viewer === state) {
          report('Remote control channel closed; reconnect the viewer');
          void disconnect(); // End ownership so prtsd releases held keys/buttons.
        }
      };
      channel.onmessage = event => {
        if (viewer !== state) return;
        try {
          if (typeof event.data !== 'string' || new TextEncoder().encode(event.data).length > 65536) throw new Error('Invalid DataChannel record');
          const message = JSON.parse(event.data);
          if (message.v !== 1) throw new Error('Invalid DataChannel version');
          if (message.type === 'ready') {
            if (message.session_id !== state.id || message.instance_id !== instance) throw new Error('Wrong session');
            input.control = session.control && message.control;
            showTarget(message.target);
          } else if (message.type === 'target') showTarget(message.target);
          else if (message.type === 'error') report(message.error);
          else if (message.type === 'cursor' && input.target?.target_id === message.target_id && input.target?.generation === message.generation) input.setCursor(message.cursor);
        } catch (error) { report(error); channel.close(); }
      };
    };
    const offer = await rpc('session.offer', { session_id: state.id });
    if (viewer !== state) return;
    await pc.setRemoteDescription(offer.description);
    state.remoteReady = true;
    for (const candidate of state.remoteIce.splice(0)) await addRemoteIce(state, candidate);
    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);
    await rpc('session.answer', { session_id: state.id, description: { type: 'answer', sdp: pc.localDescription.sdp } });
    state.stats = new ReceiverStats(pc, () => state.feedback, sample => {
      if (viewer !== state) return;
      const n = (value, digits = 1) => value === null ? 'unavailable' : value.toFixed(digits);
      $('quality').textContent = [
        `RTT: ${n(sample.rtt_ms)} ms    FPS: ${n(sample.decoded_fps)}`,
        `Received: ${n(sample.received_bps === null ? null : sample.received_bps / 1000)} kbps`,
        `Packets lost (interval): ${sample.packets_lost ?? 'unavailable'}    Jitter: ${n(sample.jitter_ms)} ms`,
        `Decode: ${n(sample.decode_ms_avg)} ms/frame    Jitter buffer: ${n(sample.jitter_buffer_ms)} ms/frame`,
        `Frames dropped (interval): ${sample.frames_dropped ?? 'unavailable'}    Codec: ${sample.codec}`,
        'Capture-to-display latency is unavailable in v1.',
      ].join('\n');
    });
  } catch (error) { await disconnect(); throw error; }
  finally { connecting = false; }
}
function addRemoteIce(state, candidate) {
  state.iceChain = state.iceChain.then(async () => {
    if (state.closed) return;
    const ufrags = [...state.pc.remoteDescription.sdp.matchAll(/^a=ice-ufrag:(.+)$/gm)].map(m => m[1].trim());
    if (candidate && !ufrags.includes(candidate.usernameFragment)) return;
    await state.pc.addIceCandidate(candidate);
  });
  return state.iceChain;
}

ws.onopen = run(async () => {
  const hello = await rpc('hello', { versions: [1] });
  $('status').textContent = `Control plane connected · ${hello.daemon} v${hello.version}`;
  await refreshInstances();
});
ws.onclose = event => {
  $('status').textContent = event.code === 1011 ? 'prtsd unavailable; restart the Node server' : 'Control plane disconnected; reload to reconnect';
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error('Control plane disconnected')); }
  pending.clear();
  void disconnect();
};
ws.onerror = () => report('WebSocket connection failed');
ws.onmessage = e => {
  const message = JSON.parse(e.data);
  if (message.id) {
    const p = pending.get(message.id); if (!p) return;
    clearTimeout(p.timer); pending.delete(message.id);
    if (message.error) p.reject(Object.assign(new Error(message.error.message), message.error));
    else p.resolve(message.result);
    return;
  }
  const { event, data } = message;
  const instance = instances.get(data.instance_id);
  if (event === 'instance.ready') {
    if (instance) Object.assign(instance, { state: 'ready', active_target_id: data.active_target_id });
    if (selected === data.instance_id) run(async () => { await refreshTargets(); if (!viewer) await connect(); })();
  } else if (event === 'instance.failed') {
    if (instance) instance.state = 'failed';
    if (selected === data.instance_id) { report(data.error); void disconnect(); }
  } else if (event === 'instance.closed') {
    instances.delete(data.instance_id);
    if (selected === data.instance_id) { void disconnect(); selected = null; targets.clear(); renderTargets(); }
  }
  if (event.startsWith('instance.')) renderInstances();
  if (data.instance_id === selected) {
    if (event === 'target.created' || event === 'target.changed') { targets.set(data.target.target_id, data.target); renderTargets(); }
    if (event === 'target.closed') { targets.delete(data.target_id); renderTargets(); }
    if (event === 'stats') $('encoder').textContent = JSON.stringify(data, null, 2);
  }
  if (viewer && data.session_id === viewer.id) {
    if (event === 'session.ice') {
      if (!viewer.remoteReady) {
        if (viewer.remoteIce.length >= 256) { report('Too many ICE candidates'); void disconnect(); return; }
        viewer.remoteIce.push(data.candidate);
      } else addRemoteIce(viewer, data.candidate).catch(report);
    } else if (event === 'session.closed') { report(`Session closed: ${data.reason}`); void disconnect(); }
  }
};

$('create').onsubmit = e => {
  e.preventDefault();
  run(async () => {
    const form = e.target;
    const result = await rpc('instance.create', { options: { initial_url: form.elements.url.value,
      size: sizeFrom(form), source: form.elements.source.value, encoder: form.elements.encoder.value } });
    instances.set(result.instance.instance_id, result.instance); renderInstances();
    await selectInstance(result.instance.instance_id);
  })();
};
$('refresh').onclick = run(refreshInstances);
$('connect').onclick = run(connect);
$('disconnect').onclick = run(disconnect);
$('navigate').onsubmit = e => { e.preventDefault(); run(() => rpc('target.navigate', { ...requireTarget(), url: e.target.elements.url.value }))(); };
$('new-tab').onclick = run(() => rpc('target.create', { instance_id: requireInstance(), url: $('navigate').elements.url.value, activate: true }));
$('close-tab').onclick = run(() => rpc('target.close', requireTarget()));
$('resize').onsubmit = e => { e.preventDefault(); run(() => rpc('target.resize', { ...requireTarget(), size: sizeFrom(e.target) }))(); };
$('network').onsubmit = e => {
  e.preventDefault();
  run(async () => {
    const f = e.target.elements;
    await rpc('network.set', { instance_id: requireInstance(), settings: {
      bitrate_cap_bps: f.cap.value === '' ? null : Number(f.cap.value) * 1000,
      added_latency_ms: Number(f.latency.value), loss_percent: Number(f.loss.value), seed: Number(f.seed.value),
    } });
    $('session-state').textContent = 'Network settings accepted by prtsd';
  })();
};
window.addEventListener('pagehide', () => ws.close());
