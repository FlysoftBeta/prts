import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import { Decoder, encode, Writer, MAX_RECORD } from '../framing.js';
import { mapPoint, RemoteInput } from '../public/input.js';
import { visibleSize, cropLayout } from '../public/video-crop.js';
import { receiverSample } from '../public/stats.js';
import { Daemon } from '../daemon.js';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';

test('framing handles bytewise UTF-8, coalescing, invalid lengths, version and truncation', () => {
  const messages = [], decoder = new Decoder(m => messages.push(m));
  const a = { v: 1, id: 'a', result: { text: '你好\n🌍' } }, b = { v: 1, id: 'b', result: {} };
  for (const byte of encode(a)) decoder.push(Buffer.from([byte]));
  decoder.push(Buffer.concat([encode(b), encode(a)])); decoder.end();
  assert.deepEqual(messages, [a, b, a]);
  for (const n of [0, MAX_RECORD + 1]) {
    const prefix = Buffer.alloc(4); prefix.writeUInt32LE(n);
    assert.throws(() => new Decoder(() => {}).push(prefix), /length/);
  }
  assert.throws(() => new Decoder(() => {}).push(encode({ v: 2 })), /version/);
  const truncated = new Decoder(() => {}); truncated.push(encode(a).subarray(0, 6));
  assert.throws(() => truncated.end(), /Truncated/);
  assert.throws(() => new Decoder(() => {}).push(Buffer.from([1, 0, 0, 0, 0xff])));
});

test('writes serialize through a slow pipe', async () => {
  const pipe = new PassThrough({ highWaterMark: 1 }), received = [];
  const decoder = new Decoder(m => received.push(m));
  const writer = new Writer(pipe);
  const writes = [writer.write({ v: 1, id: 'a' }), writer.write({ v: 1, id: 'b' })];
  pipe.on('data', chunk => decoder.push(chunk));
  await Promise.all(writes);
  assert.deepEqual(received.map(m => m.id), ['a', 'b']);
  pipe.end();
});

test('pointer mapping excludes letterbox bars, uses CSS pixels and permits captured drags', () => {
  const rect = { left: 10, top: 20, width: 1000, height: 1000 }, size = { width: 1280, height: 720, dpr: 2 };
  assert.equal(mapPoint(rect, 2560, 1440, size, 510, 30), null);
  assert.deepEqual(mapPoint(rect, 2560, 1440, size, 510, 520), { x: 640, y: 360 });
  assert.ok(mapPoint(rect, 2560, 1440, size, 0, 0, true).x < 0);
  assert.equal(mapPoint(rect, 2560, 1440, size, 1010, 520), null);
  assert.equal(mapPoint(rect, 2560, 1440, size, 100000, 520, true).x, 32767);
});

test('odd video crop preserves the visible aspect ratio and pointer coordinates', () => {
  const size = { width: 1203, height: 803, dpr: 1 };
  assert.deepEqual(visibleSize(1204, 804, size), [1203, 803]);
  assert.deepEqual(visibleSize(960, 640, size), [960, 640]); // old frame during resize
  const layout = cropLayout(1203, 803, 1204, 804, size);
  assert.equal(layout.width, '1204px'); assert.equal(layout.height, '804px');
  assert.equal(layout.left, '0px'); assert.equal(layout.top, '0px');
  const rect = { left: 0, top: 0, width: 1204, height: 804 };
  assert.deepEqual(mapPoint(rect, 1204, 804, size, 1202.5, 802.5), { x: 1202.5, y: 802.5 });
  assert.equal(mapPoint(rect, 1204, 804, size, 1203.5, 802), null);
  assert.equal(mapPoint(rect, 1204, 804, size, 1202, 803.5), null);
});

test('receiver uses interval deltas and keeps unavailable and negative loss values', () => {
  const old = { framesDecoded: 100, bytesReceived: 1000, packetsLost: 3, totalDecodeTime: 1, jitterBufferDelay: 2, jitterBufferEmittedCount: 100, framesDropped: 2 };
  const now = { framesDecoded: 130, bytesReceived: 11000, packetsLost: 2, totalDecodeTime: 1.06, jitterBufferDelay: 2.3, jitterBufferEmittedCount: 130, framesDropped: 4, jitter: 0.004 };
  const sample = receiverSample(now, old, 1000, 20);
  assert.equal(sample.decoded_fps, 30); assert.equal(sample.received_bps, 80000);
  assert.equal(sample.packets_lost, -1); assert.equal(sample.frames_dropped, 2);
  assert.ok(Math.abs(sample.decode_ms_avg - 2) < 1e-9); assert.ok(Math.abs(sample.jitter_buffer_ms - 10) < 1e-9);
  assert.equal(receiverSample({}, {}, 1000, undefined).decode_ms_avg, null);
});

test('input sends committed text once, normalizes wheel and clears state on blur/switch', t => {
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  globalThis.window = new EventTarget(); globalThis.document = { activeElement: null };
  t.after(() => { globalThis.window = previousWindow; globalThis.document = previousDocument; });
  const video = Object.assign(new EventTarget(), {
    videoWidth: 2560, videoHeight: 1440, style: {}, captured: null,
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 1000, height: 1000 }),
    setPointerCapture(id) { this.captured = id; },
    hasPointerCapture(id) { return this.captured === id; },
    releasePointerCapture() { this.captured = null; },
  });
  const keyboard = Object.assign(new EventTarget(), {
    value: '', focus() { document.activeElement = this; this.dispatchEvent(new Event('focus')); },
    blur() { document.activeElement = null; this.dispatchEvent(new Event('blur')); },
  });
  const cursor = { style: {} }, sent = [];
  const input = new RemoteInput(video, keyboard, cursor, assert.fail);
  const emit = (element, type, props) => {
    const event = Object.assign(new Event(type, { cancelable: true }), props);
    element.dispatchEvent(event); return event;
  };
  input.channel = { readyState: 'open', bufferedAmount: 0, send: text => sent.push(JSON.parse(text)) };
  input.control = true;
  input.setTarget({ target_id: '1', generation: '1', size: { width: 1280, height: 720, dpr: 2 } });
  const pointer = { pointerType: 'mouse', pointerId: 1, clientX: 500, clientY: 500, button: 0, buttons: 1 };
  emit(video, 'pointerdown', pointer);
  assert.equal(sent.at(-1).x, 640); assert.equal(sent.at(-1).y, 360); assert.equal(sent.at(-1).click_count, 1);
  const key = { key: 'a', code: 'KeyA', keyCode: 65, location: 0, repeat: false, altKey: false };
  emit(keyboard, 'keydown', key);
  assert.equal(sent.at(-1).type, 'key.down'); assert.equal(sent.filter(m => m.type === 'key.char').length, 0);
  emit(keyboard, 'input', { data: 'a', isComposing: false });
  emit(keyboard, 'keyup', key);
  emit(keyboard, 'compositionstart', {});
  emit(keyboard, 'input', { data: '你', isComposing: true });
  emit(keyboard, 'compositionend', { data: '你好🌍' });
  emit(keyboard, 'input', { data: '你好🌍', isComposing: false });
  assert.deepEqual(sent.filter(m => m.type === 'key.char').map(m => m.text), ['a', '你好🌍']);
  const wheel = { clientX: 500, clientY: 500, deltaMode: 2, deltaX: 1, deltaY: 2 };
  assert.ok(emit(video, 'wheel', wheel).defaultPrevented);
  assert.equal(sent.at(-1).delta_x, 1280); assert.equal(sent.at(-1).delta_y, 1440);
  const count = sent.length;
  emit(video, 'wheel', { ...wheel, clientY: 0 }); assert.equal(sent.length, count);
  keyboard.blur(); assert.equal(sent.at(-1).type, 'focus'); assert.equal(sent.at(-1).focused, false);
  assert.equal(input.buttons, 0); assert.equal(input.keys.size, 0); assert.equal(video.captured, null);
  input.setTarget({ target_id: '2', generation: '4', size: { width: 640, height: 360, dpr: 1 } });
  input.commit('after switch'); assert.equal(sent.at(-1).target_id, '2'); assert.equal(sent.at(-1).generation, '4');
  assert.deepEqual(sent.map(m => m.sequence), sent.map((_, index) => index + 1));
  clearTimeout(input.compositionTimer);
});

test('daemon exit rejects later requests cleanly', { timeout: 5000 }, async t => {
  t.mock.method(console, 'error', () => {});
  const daemon = new Daemon(process.execPath, [fileURLToPath(new URL('./mock-prtsd.js', import.meta.url))], fileURLToPath(new URL('../../', import.meta.url)));
  t.after(() => daemon.child.kill());
  await daemon.ready;
  const unavailable = once(daemon, 'unavailable');
  daemon.child.kill(); await unavailable;
  await assert.rejects(daemon.request('instance.list'), { code: 'invalid_state' });
  assert.equal(daemon.pending.size, 0);
});
