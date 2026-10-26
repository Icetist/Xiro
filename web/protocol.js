// Wire protocol between the PC and the Safari viewer.
// Binary messages start with a one-byte type tag; control messages are JSON text frames.

export const MSG = Object.freeze({
  VIDEO: 0x01,
  POINTER: 0x10,
  WHEEL: 0x11,
});

export const POINTER_KIND = Object.freeze({ TOUCH: 0, PEN: 1, MOUSE: 2 });
export const PHASE = Object.freeze({ DOWN: 0, MOVE: 1, UP: 2, CANCEL: 3, HOVER: 4 });

// VIDEO: [type u8][flags u8][sentAt f64 LE (ms, host clock)][AVCC sample...]
export const VIDEO_HEADER_BYTES = 10;
export const VIDEO_FLAG_KEY = 0x01;
/** Catch-up frame re-sent from the host's GOP cache; decode even if behind. */
export const VIDEO_FLAG_REPLAY = 0x02;

// POINTER: [type u8][kind u8][phase u8][id u8][x u16][y u16][pressure u16][tiltX i8][tiltY i8][buttons u8]
export const POINTER_BYTES = 14;
// WHEEL: [type u8][pad u8][x u16][y u16][dx i16][dy i16]
export const WHEEL_BYTES = 10;

const U16_MAX = 0xffff;

const clamp01 = (v) => (v < 0 ? 0 : v > 1 ? 1 : v);
const clampI8 = (v) => Math.max(-90, Math.min(90, Math.round(v || 0)));
const clampI16 = (v) => Math.max(-32768, Math.min(32767, Math.round(v || 0)));

export function writeVideoHeader(view, isKey, sentAt) {
  view.setUint8(0, MSG.VIDEO);
  view.setUint8(1, isKey ? VIDEO_FLAG_KEY : 0);
  view.setFloat64(2, sentAt, true);
}

export function readVideoHeader(view) {
  const flags = view.getUint8(1);
  return {
    isKey: (flags & VIDEO_FLAG_KEY) !== 0,
    isReplay: (flags & VIDEO_FLAG_REPLAY) !== 0,
    sentAt: view.getFloat64(2, true),
  };
}

/** x, y and pressure are normalised to [0, 1]; tilt is in degrees. */
export function encodePointer({ kind, phase, id, x, y, pressure = 0, tiltX = 0, tiltY = 0, buttons = 0 }) {
  const buf = new ArrayBuffer(POINTER_BYTES);
  const v = new DataView(buf);
  v.setUint8(0, MSG.POINTER);
  v.setUint8(1, kind);
  v.setUint8(2, phase);
  v.setUint8(3, id & 0xff);
  v.setUint16(4, Math.round(clamp01(x) * U16_MAX), true);
  v.setUint16(6, Math.round(clamp01(y) * U16_MAX), true);
  v.setUint16(8, Math.round(clamp01(pressure) * U16_MAX), true);
  v.setInt8(10, clampI8(tiltX));
  v.setInt8(11, clampI8(tiltY));
  v.setUint8(12, buttons & 0xff);
  return buf;
}

const KINDS = new Set(Object.values(POINTER_KIND));
const PHASES = new Set(Object.values(PHASE));

/** Returns null for truncated or out-of-range messages. */
export function decodePointer(view) {
  if (view.byteLength < POINTER_BYTES - 1) return null;
  const kind = view.getUint8(1);
  const phase = view.getUint8(2);
  if (!KINDS.has(kind) || !PHASES.has(phase)) return null;
  return {
    kind,
    phase,
    id: view.getUint8(3),
    x: view.getUint16(4, true) / U16_MAX,
    y: view.getUint16(6, true) / U16_MAX,
    pressure: view.getUint16(8, true) / U16_MAX,
    tiltX: clampI8(view.getInt8(10)),
    tiltY: clampI8(view.getInt8(11)),
    buttons: view.getUint8(12),
  };
}

/** dx/dy are in wheel "notches" * 120 (Windows WHEEL_DELTA units). */
export function encodeWheel({ x, y, dx, dy }) {
  const buf = new ArrayBuffer(WHEEL_BYTES);
  const v = new DataView(buf);
  v.setUint8(0, MSG.WHEEL);
  v.setUint16(2, Math.round(clamp01(x) * U16_MAX), true);
  v.setUint16(4, Math.round(clamp01(y) * U16_MAX), true);
  v.setInt16(6, clampI16(dx), true);
  v.setInt16(8, clampI16(dy), true);
  return buf;
}

export function decodeWheel(view) {
  if (view.byteLength < WHEEL_BYTES) return null;
  return {
    x: view.getUint16(2, true) / U16_MAX,
    y: view.getUint16(4, true) / U16_MAX,
    dx: view.getInt16(6, true),
    dy: view.getInt16(8, true),
  };
}
