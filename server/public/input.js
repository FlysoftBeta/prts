import { visibleSize } from './video-crop.js';

export function mapPoint(rect, videoWidth, videoHeight, size, clientX, clientY, captured = false) {
  if (!rect.width || !rect.height || !videoWidth || !videoHeight) return null;
  const scale = Math.min(rect.width / videoWidth, rect.height / videoHeight);
  const [w, h] = visibleSize(videoWidth, videoHeight, size);
  const width = w * scale, height = h * scale;
  const left = rect.left + (rect.width - videoWidth * scale) / 2, top = rect.top + (rect.height - videoHeight * scale) / 2;
  const px = clientX - left, py = clientY - top;
  if (!captured && (px < 0 || py < 0 || px >= width || py >= height)) return null;
  const clamp = n => Math.max(-32768, Math.min(32767, n));
  return { x: clamp(px * size.width / width), y: clamp(py * size.height / height) };
}

export const modifiers = e => (e.shiftKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.altKey ? 4 : 0) |
  (e.metaKey ? 8 : 0) | (e.getModifierState?.('CapsLock') ? 16 : 0) | (e.getModifierState?.('NumLock') ? 32 : 0);

export class RemoteInput {
  target = null;
  channel = null;
  control = false;
  sequence = 0;
  buttons = 0;
  pointer = null;
  keys = new Set();
  counts = new Map();
  clicks = new Map();
  composing = false;
  constructor(video, keyboard, cursor, report) {
    Object.assign(this, { video, keyboard, cursor, report });
    video.addEventListener('contextmenu', e => e.preventDefault());
    video.addEventListener('pointerdown', e => {
      if (e.pointerType !== 'mouse' || !this.control || !this.point(e)) return;
      e.preventDefault();
      keyboard.focus({ preventScroll: true });
      video.setPointerCapture(e.pointerId);
      this.pointer = e.pointerId;
      this.buttons = e.buttons;
      const last = this.clicks.get(e.button);
      const count = last && e.timeStamp - last.time < 500 && Math.hypot(e.clientX - last.x, e.clientY - last.y) < 5 ? last.count % 3 + 1 : 1;
      this.clicks.set(e.button, { time: e.timeStamp, x: e.clientX, y: e.clientY, count });
      this.counts.set(e.button, count);
      this.send('mouse.down', { ...this.point(e), button: e.button + 1, buttons: e.buttons, click_count: count }, e);
    });
    video.addEventListener('pointermove', e => {
      if (e.pointerType !== 'mouse') return;
      const point = this.point(e, this.pointer === e.pointerId);
      if (!point) { cursor.hidden = true; return; }
      const rect = (video.parentElement ?? video).getBoundingClientRect();
      cursor.style.left = `${e.clientX - rect.left - (video.parentElement?.clientLeft || 0) - (this.hotX || 0)}px`;
      cursor.style.top = `${e.clientY - rect.top - (video.parentElement?.clientTop || 0) - (this.hotY || 0)}px`;
      cursor.hidden = !this.customCursor;
      this.send('mouse.move', { ...point, buttons: this.buttons }, e);
    });
    video.addEventListener('pointerleave', () => { cursor.hidden = true; });
    video.addEventListener('pointerup', e => {
      if (this.pointer !== e.pointerId) return;
      e.preventDefault();
      this.buttons = e.buttons;
      this.send('mouse.up', { ...this.point(e, true), button: e.button + 1, buttons: e.buttons, click_count: this.counts.get(e.button) || 1 }, e);
      if (!e.buttons) { this.pointer = null; video.releasePointerCapture(e.pointerId); }
    });
    video.addEventListener('pointercancel', () => this.release());
    video.addEventListener('lostpointercapture', () => { if (this.buttons) this.release(); });
    video.addEventListener('wheel', e => {
      const point = this.point(e);
      if (!point || !this.control) return;
      e.preventDefault();
      const sx = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? this.target.size.width : 1;
      const sy = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? this.target.size.height : 1;
      this.send('mouse.wheel', { ...point, delta_x: e.deltaX * sx, delta_y: e.deltaY * sy, buttons: this.buttons }, e);
    }, { passive: false });
    keyboard.addEventListener('focus', e => this.send('focus', { focused: true }, e));
    keyboard.addEventListener('blur', () => this.release());
    window.addEventListener('blur', () => { this.release(); keyboard.blur(); });
    keyboard.addEventListener('keydown', e => {
      if (e.key === 'Escape' && !e.isComposing) { e.preventDefault(); keyboard.blur(); return; }
      const printable = [...e.key].length === 1;
      const text = printable && !e.ctrlKey && !e.metaKey && !e.altKey ? e.key : '';
      this.send('key.down', this.keyFields(e, text), e);
      this.keys.add(e.code);
      // Let the textarea perform text/IME/clipboard input; block local editing keys.
      if (!e.isComposing && !printable && !['Process', 'Dead', 'Unidentified', 'Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) e.preventDefault();
    });
    keyboard.addEventListener('keyup', e => {
      if (!this.keys.delete(e.code)) return;
      this.send('key.up', { ...this.keyFields(e, ''), repeat: false }, e);
    });
    keyboard.addEventListener('compositionstart', () => { this.composing = true; });
    keyboard.addEventListener('compositionend', e => {
      this.composing = false;
      this.commit(e.data);
      keyboard.value = '';
      this.lastComposition = e.data;
      clearTimeout(this.compositionTimer);
      this.compositionTimer = setTimeout(() => { this.lastComposition = null; }, 0);
    });
    keyboard.addEventListener('input', e => {
      if (this.composing || e.isComposing) return;
      const text = e.data ?? keyboard.value;
      if (this.lastComposition !== text) this.commit(text);
      this.lastComposition = null;
      keyboard.value = '';
    });
  }
  keyFields(e, text) {
    return { ...(e.keyCode > 0 && e.keyCode <= 255 ? { vk: e.keyCode } : {}), code: e.code, key: e.key,
      text, location: e.location, repeat: e.repeat, is_system: e.altKey };
  }
  commit(text) {
    if (!text) return;
    // Split pasted/IME text at scalar boundaries within the 4096-byte wire limit.
    let chunk = '', bytes = 0;
    for (const char of text.replaceAll('\0', '').toWellFormed()) {
      const n = new TextEncoder().encode(char).length;
      if (bytes + n > 4096) { this.send('key.char', { text: chunk }); chunk = ''; bytes = 0; }
      chunk += char; bytes += n;
    }
    if (chunk) this.send('key.char', { text: chunk });
  }
  point(e, captured = false) {
    return this.target && mapPoint(this.video.getBoundingClientRect(), this.video.videoWidth, this.video.videoHeight, this.target.size, e.clientX, e.clientY, captured);
  }
  send(type, fields, event = {}) {
    if (!this.target || !this.control || this.channel?.readyState !== 'open') return;
    if (this.channel.bufferedAmount > 256 * 1024) {
      this.report('Input channel is congested; reconnect the viewer');
      this.channel.close(); // Closing releases held remote state; never drop a transition.
      return;
    }
    this.channel.send(JSON.stringify({ v: 1, type, target_id: this.target.target_id, generation: this.target.generation,
      sequence: ++this.sequence, modifiers: modifiers(event), ...fields }));
  }
  release() {
    this.send('focus', { focused: false });
    this.buttons = 0;
    this.keys.clear();
    if (this.pointer !== null && this.video.hasPointerCapture(this.pointer)) this.video.releasePointerCapture(this.pointer);
    this.pointer = null;
    this.keyboard.value = '';
  }
  setTarget(target) {
    this.release();
    this.target = target;
    this.setCursor({ shape: 'default' });
    if (document.activeElement === this.keyboard) this.send('focus', { focused: true });
  }
  setCursor(cursor) {
    this.customCursor = false;
    this.cursor.hidden = true;
    const shapes = ['default', 'pointer', 'text', 'crosshair', 'move', 'ew-resize', 'ns-resize', 'none'];
    if (shapes.includes(cursor.shape)) this.video.style.cursor = cursor.shape;
    else if (cursor.shape === 'custom' && this.target && cursor.width > 0 && cursor.width <= 128 && cursor.height > 0 && cursor.height <= 128 &&
      cursor.hot_x >= 0 && cursor.hot_x < cursor.width && cursor.hot_y >= 0 && cursor.hot_y < cursor.height &&
      typeof cursor.png_base64 === 'string' && cursor.png_base64.length <= 43692 && /^[A-Za-z0-9+/]*={0,2}$/.test(cursor.png_base64)) {
      const dpr = this.target.size.dpr || 1;
      this.cursor.src = `data:image/png;base64,${cursor.png_base64}`;
      this.cursor.style.width = `${cursor.width / dpr}px`;
      this.cursor.style.height = `${cursor.height / dpr}px`;
      this.hotX = cursor.hot_x / dpr; this.hotY = cursor.hot_y / dpr;
      this.customCursor = true;
      this.video.style.cursor = 'none';
    } else this.video.style.cursor = 'default';
  }
}
