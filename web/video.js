// Hardware H.264 decode with WebCodecs, drawn straight to a canvas.
// No <video> element, no jitter buffer: each frame is shown the moment it decodes.

import { VIDEO_HEADER_BYTES, readVideoHeader } from './protocol.js';

/** Decoder backlog (frames) beyond which we skip to the next key frame instead of queueing. */
const MAX_DECODE_QUEUE = 2;
const MAX_TRACKED_FRAMES = 64;
/** Frames held while the decoder is being (re)configured. */
const MAX_PENDING_FRAMES = 240;

function base64ToBytes(b64) {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class VideoPipeline {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {{
   *   onFrame: (info: { sentAt: number, decodeMs: number, width: number, height: number }) => void,
   *   onError: (msg: string) => void,
   *   onNeedKey: () => void,
   * }} handlers
   */
  constructor(canvas, { onFrame, onError, onNeedKey }) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
    this.onFrame = onFrame;
    this.onError = onError;
    this.onNeedKey = onNeedKey;
    this.decoder = null;
    this.decoderConfig = null;
    this.configuring = false;
    this.pending = [];
    this.waitingForKey = true;
    this.catchingUp = false;
    this.submittedAt = new Map();
    this.stats = { bytes: 0, received: 0, decoded: 0, skipped: 0 };
  }

  static isSupported() {
    return typeof window.VideoDecoder === 'function' && typeof window.EncodedVideoChunk === 'function';
  }

  /** @param {{ codec: string, description: string }} cfg description is base64 avcC */
  async configure({ codec, description }) {
    // Frames for the new stream can arrive while we await support checks; hold them.
    this.configuring = true;
    this.pending = [];
    const base = { codec, description: base64ToBytes(description), optimizeForLatency: true };
    let config = { ...base, hardwareAcceleration: 'prefer-hardware' };
    try {
      if (!(await VideoDecoder.isConfigSupported(config)).supported) {
        config = base;
        if (!(await VideoDecoder.isConfigSupported(base)).supported) {
          this.configuring = false;
          this.onError(`This device cannot decode ${codec}.`);
          return;
        }
      }
    } catch {
      config = base;
    }
    this.#createDecoder(config);
    this.configuring = false;
    const held = this.pending;
    this.pending = [];
    for (const buffer of held) this.push(buffer);
  }

  #createDecoder(config) {
    if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
    this.decoderConfig = config;
    this.waitingForKey = true;
    this.submittedAt.clear();
    this.decoder = new VideoDecoder({
      output: (frame) => this.#present(frame),
      error: (err) => {
        this.onError(`Decoder error: ${err.message}`);
        // Start over and ask the host for the current GOP.
        if (this.decoderConfig) this.#createDecoder(this.decoderConfig);
        this.onNeedKey();
      },
    });
    this.decoder.configure(config);
  }

  #skipUntilKey() {
    this.stats.skipped += 1;
    if (!this.waitingForKey) {
      this.waitingForKey = true;
      this.onNeedKey();
    }
  }

  /** @param {ArrayBuffer} buffer a VIDEO message */
  push(buffer) {
    if (this.configuring) {
      if (this.pending.length < MAX_PENDING_FRAMES) this.pending.push(buffer);
      return;
    }
    const { isKey, isReplay, sentAt } = readVideoHeader(new DataView(buffer, 0, VIDEO_HEADER_BYTES));
    this.stats.received += 1;
    this.stats.bytes += buffer.byteLength;
    const decoder = this.decoder;
    if (!decoder || decoder.state !== 'configured') return;

    // After a replay the queue is legitimately full of catch-up frames; let it drain once
    // before treating a deep queue as a backlog worth skipping.
    if (isReplay) this.catchingUp = true;
    else if (this.catchingUp && decoder.decodeQueueSize <= MAX_DECODE_QUEUE) this.catchingUp = false;

    if (!isKey) {
      if (this.waitingForKey) return this.#skipUntilKey();
      if (!this.catchingUp && decoder.decodeQueueSize > MAX_DECODE_QUEUE) return this.#skipUntilKey();
    }
    this.waitingForKey = false;

    // The host send time doubles as the frame timestamp (µs), so latency can be computed on output.
    const timestamp = Math.round(sentAt * 1000);
    if (this.submittedAt.size > MAX_TRACKED_FRAMES) this.submittedAt.clear();
    this.submittedAt.set(timestamp, performance.now());
    decoder.decode(new EncodedVideoChunk({
      type: isKey ? 'key' : 'delta',
      timestamp,
      data: new Uint8Array(buffer, VIDEO_HEADER_BYTES),
    }));
  }

  #present(frame) {
    const width = frame.displayWidth;
    const height = frame.displayHeight;
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
    this.ctx.drawImage(frame, 0, 0, width, height);
    const started = this.submittedAt.get(frame.timestamp);
    this.submittedAt.delete(frame.timestamp);
    this.stats.decoded += 1;
    this.onFrame({ sentAt: frame.timestamp / 1000, decodeMs: started ? performance.now() - started : 0, width, height });
    frame.close();
  }

  close() {
    if (this.decoder && this.decoder.state !== 'closed') this.decoder.close();
    this.decoder = null;
  }
}
