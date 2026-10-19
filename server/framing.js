import { TextDecoder } from 'node:util';

export const MAX_RECORD = 4_194_304;
export const MAX_QUEUE = 8 * 1024 * 1024;

export function encode(message) {
  const body = Buffer.from(JSON.stringify(message), 'utf8');
  if (!body.length || body.length > MAX_RECORD) throw new Error('Invalid record length');
  const frame = Buffer.allocUnsafe(body.length + 4);
  frame.writeUInt32LE(body.length);
  body.copy(frame, 4);
  return frame;
}

// Allocate only after validating the prefix. Handles any fragmentation/coalescing.
export class Decoder {
  prefix = Buffer.alloc(4);
  used = 0;
  body = null;
  offset = 0;
  constructor(onMessage) { this.onMessage = onMessage; }
  push(chunk) {
    while (chunk.length) {
      if (!this.body) {
        const n = Math.min(4 - this.used, chunk.length);
        chunk.copy(this.prefix, this.used, 0, n);
        this.used += n;
        chunk = chunk.subarray(n);
        if (this.used < 4) continue;
        const length = this.prefix.readUInt32LE();
        if (length < 1 || length > MAX_RECORD) throw new Error('Invalid record length');
        this.body = Buffer.allocUnsafe(length);
        this.offset = 0;
      }
      const n = Math.min(this.body.length - this.offset, chunk.length);
      chunk.copy(this.body, this.offset, 0, n);
      this.offset += n;
      chunk = chunk.subarray(n);
      if (this.offset === this.body.length) {
        const message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(this.body));
        this.body = null;
        this.used = 0;
        if (!message || Array.isArray(message) || typeof message !== 'object' || message.v !== 1) {
          throw new Error('Invalid protocol object/version');
        }
        this.onMessage(message);
      }
    }
  }
  end() {
    if (this.used || this.body) throw new Error('Truncated record');
  }
}

// Writable already serializes writes and honors drain; never enqueue unbounded data.
export class Writer {
  queuedBytes = 0;
  tail = Promise.resolve();
  constructor(stream) { this.stream = stream; }
  write(message) {
    const frame = encode(message);
    if (this.stream.destroyed || this.stream.writableEnded) throw new Error('Pipe closed');
    if (this.queuedBytes + frame.length > MAX_QUEUE) throw new Error('Pipe queue overflow');
    this.queuedBytes += frame.length;
    // Wait for each write callback (which occurs after the chunk is flushed)
    // before submitting another chunk, including when write() returns false.
    const write = this.tail.then(() => new Promise((resolve, reject) => {
      this.stream.write(frame, error => error ? reject(error) : resolve());
    }));
    this.tail = write.catch(() => {});
    return write.finally(() => { this.queuedBytes -= frame.length; });
  }
}
