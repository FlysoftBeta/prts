const finite = value => Number.isFinite(value) ? value : null;
export function receiverSample(current, previous, interval, rtt) {
  const delta = field => Number.isFinite(current[field]) && Number.isFinite(previous?.[field]) ? current[field] - previous[field] : null;
  const frames = delta('framesDecoded');
  const average = (total, count) => total !== null && count > 0 ? total * 1000 / count : null;
  const bytes = delta('bytesReceived');
  return {
    interval_ms: Math.round(interval),
    decoded_fps: frames === null ? null : Math.max(0, frames * 1000 / interval),
    received_bps: bytes === null ? null : Math.max(0, bytes * 8000 / interval),
    rtt_ms: finite(rtt), jitter_ms: finite(current.jitter * 1000),
    packets_lost: delta('packetsLost'),
    decode_ms_avg: average(delta('totalDecodeTime'), frames), render_ms_avg: null,
    jitter_buffer_ms: average(delta('jitterBufferDelay'), delta('jitterBufferEmittedCount')),
    frames_dropped: delta('framesDropped'),
  };
}

export class ReceiverStats {
  previous = null;
  seq = 0;
  busy = false;
  constructor(pc, feedback, display) {
    this.pc = pc; this.feedback = feedback; this.display = display;
    this.timer = setInterval(() => this.sample().catch(() => {}), 1000);
  }
  stop() { clearInterval(this.timer); }
  async sample() {
    if (this.busy) return;
    this.busy = true;
    try {
      const reports = await this.pc.getStats();
      const inbound = [...reports.values()].find(r => r.type === 'inbound-rtp' && (r.kind === 'video' || r.mediaType === 'video'));
      if (!inbound) return;
      const transport = reports.get(inbound.transportId);
      const pair = reports.get(transport?.selectedCandidatePairId) || [...reports.values()].find(r => r.type === 'candidate-pair' && r.nominated && r.state === 'succeeded');
      const previous = this.previous?.id === inbound.id ? this.previous : null;
      this.previous = inbound;
      if (!previous) return;
      const interval = inbound.timestamp - previous.timestamp;
      if (interval < 1 || interval > 60000) return;
      const sample = receiverSample(inbound, previous, interval, pair?.currentRoundTripTime * 1000);
      const codec = reports.get(inbound.codecId);
      this.display({ ...sample, codec: codec?.mimeType || 'unavailable' });
      const channel = this.feedback();
      if (channel?.readyState === 'open' && channel.bufferedAmount < 65536 && sample.decoded_fps !== null && sample.received_bps !== null && sample.packets_lost !== null) {
        const { jitter_buffer_ms, frames_dropped, ...wire } = sample;
        channel.send(JSON.stringify({ v: 1, type: 'stats', sample_seq: ++this.seq, ...wire }));
      }
    } finally { this.busy = false; }
  }
}
