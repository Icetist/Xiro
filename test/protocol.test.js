import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MSG, PHASE, POINTER_KIND, VIDEO_HEADER_BYTES,
  encodePointer, decodePointer, encodeWheel, decodeWheel, writeVideoHeader, readVideoHeader,
} from '../web/protocol.js';

test('pointer messages round-trip within u16 precision', () => {
  const buf = encodePointer({ kind: POINTER_KIND.PEN, phase: PHASE.MOVE, id: 3, x: 0.25, y: 0.75, pressure: 0.5, tiltX: -30, tiltY: 45, buttons: 1 });
  const view = new DataView(buf);
  assert.equal(view.getUint8(0), MSG.POINTER);

  const p = decodePointer(view);
  assert.equal(p.kind, POINTER_KIND.PEN);
  assert.equal(p.phase, PHASE.MOVE);
  assert.equal(p.id, 3);
  assert.ok(Math.abs(p.x - 0.25) < 1e-4);
  assert.ok(Math.abs(p.y - 0.75) < 1e-4);
  assert.ok(Math.abs(p.pressure - 0.5) < 1e-4);
  assert.equal(p.tiltX, -30);
  assert.equal(p.tiltY, 45);
  assert.equal(p.buttons, 1);
});

test('pointer coordinates and tilt are clamped to valid ranges', () => {
  const p = decodePointer(new DataView(encodePointer({ kind: 0, phase: 0, id: 0, x: -1, y: 2, tiltX: 500, tiltY: -500 })));
  assert.equal(p.x, 0);
  assert.equal(p.y, 1);
  assert.equal(p.tiltX, 90);
  assert.equal(p.tiltY, -90);
});

test('wheel messages round-trip', () => {
  const w = decodeWheel(new DataView(encodeWheel({ x: 0.5, y: 0.5, dx: -120, dy: 240 })));
  assert.equal(w.dx, -120);
  assert.equal(w.dy, 240);
});

test('truncated messages decode to null', () => {
  assert.equal(decodePointer(new DataView(new ArrayBuffer(4))), null);
  assert.equal(decodeWheel(new DataView(new ArrayBuffer(4))), null);
});

test('video header stores key flag and timestamp', () => {
  const view = new DataView(new ArrayBuffer(VIDEO_HEADER_BYTES));
  writeVideoHeader(view, true, 12345.678);
  assert.deepEqual(readVideoHeader(view), { isKey: true, isReplay: false, sentAt: 12345.678 });
});

test('out-of-range pointer kind, phase and tilt are rejected or clamped', () => {
  const buf = encodePointer({ kind: 0, phase: 0, id: 0, x: 0, y: 0 });
  const v = new DataView(buf);
  v.setUint8(1, 9);
  assert.equal(decodePointer(v), null);
  v.setUint8(1, 0);
  v.setUint8(2, 42);
  assert.equal(decodePointer(v), null);
  v.setUint8(2, 0);
  v.setInt8(10, -128);
  assert.equal(decodePointer(v).tiltX, -90);
});
