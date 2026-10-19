import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { Decoder, Writer } from './framing.js';

export function fault(code, message) {
  return Object.assign(new Error(message), { code, retryable: false });
}

export class Daemon extends EventEmitter {
  nextId = 0;
  pending = new Map();
  sequence = 0;
  time = 0;
  dead = false;
  stopping = false;
  constructor(executable, args, cwd) {
    super();
    this.child = spawn(executable, args, { cwd, stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true });
    this.writer = new Writer(this.child.stdin);
    const decoder = new Decoder(message => this.receive(message));
    this.child.stdout.on('data', data => {
      try { decoder.push(data); } catch (error) { this.fail(error); }
    });
    this.child.stdout.on('end', () => {
      try { decoder.end(); } catch (error) { this.fail(error); }
      if (!this.stopping) this.fail(new Error('prtsd stdout closed'));
    });
    this.child.stdin.on('error', error => this.fail(error));
    this.child.stdout.on('error', error => this.fail(error));
    this.child.on('error', error => this.fail(error));
    this.child.on('exit', (code, signal) => {
      if (!this.stopping) this.fail(new Error(`prtsd exited (${signal || code})`));
    });
    this.ready = this.request('hello', { versions: [1] }).then(result => {
      if (result.version !== 1 || result.daemon !== 'prtsd') throw new Error('Incompatible hello');
      this.hello = result;
      return result;
    }).catch(error => { this.fail(error); throw error; });
    // The UI remains available to report a missing or failed daemon.
    this.ready.catch(() => {});
  }
  request(method, params = {}) {
    if (this.dead || this.stopping) return Promise.reject(fault('invalid_state', 'prtsd is unavailable; restart the server'));
    if (this.pending.size >= 1024) return Promise.reject(fault('resource_exhausted', 'Too many daemon requests'));
    const id = `n${++this.nextId}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fail(fault('timeout', 'prtsd request timed out; state is uncertain'));
      }, 5000);
      this.pending.set(id, { resolve, reject, timer, method });
      try { this.writer.write({ v: 1, id, method, params }).catch(error => this.fail(error)); }
      catch (error) { this.fail(error); }
    });
  }
  receive(message) {
    if (this.dead) return;
    if (Object.hasOwn(message, 'id')) {
      const pending = this.pending.get(message.id);
      if (!pending || Object.hasOwn(message, 'result') === Object.hasOwn(message, 'error') || message.event) {
        throw new Error('Invalid daemon response correlation');
      }
      if (message.error && (typeof message.error.code !== 'string' || typeof message.error.message !== 'string' || typeof message.error.retryable !== 'boolean')) {
        throw new Error('Invalid daemon error');
      }
      if (!message.error && (!message.result || typeof message.result !== 'object' || Array.isArray(message.result))) {
        throw new Error('Invalid daemon result');
      }
      if (pending.method === 'hello' && !message.error) {
        if (message.result?.version !== 1 || message.result?.daemon !== 'prtsd') throw new Error('Incompatible hello');
        this.hello = message.result;
      }
      clearTimeout(pending.timer);
      this.pending.delete(message.id);
      if (message.error) pending.reject(Object.assign(new Error(message.error.message), message.error));
      else pending.resolve(message.result);
      return;
    }
    if (!this.hello || typeof message.event !== 'string' || !message.data ||
        !Number.isSafeInteger(message.seq) || message.seq <= this.sequence ||
        !Number.isSafeInteger(message.time_ms) || message.time_ms < this.time) {
      throw new Error('Invalid daemon event/order');
    }
    this.sequence = message.seq;
    this.time = message.time_ms;
    this.emit('event', message);
  }
  fail(error) {
    if (this.dead) return;
    this.dead = true;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(fault(error.code || 'internal', 'prtsd connection failed; restart the server'));
    }
    this.pending.clear();
    console.error(error.message);
    this.child.stdin.destroy();
    this.child.kill();
    this.emit('unavailable');
  }
  stop() {
    if (this.stopping) return;
    this.stopping = true;
    this.child.stdin.end(); // EOF is the protocol's graceful shutdown request.
    const timer = setTimeout(() => this.child.kill(), 5000);
    timer.unref();
    this.child.once('exit', () => clearTimeout(timer));
  }
}
