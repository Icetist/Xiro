// Touch, Apple Pencil, trackpad and keyboard capture -> compact binary messages.

import { PHASE, POINTER_KIND, encodePointer, encodeWheel } from './protocol.js';

const MAX_TOUCH_SLOTS = 10;
const TAP_SLOP_PX = 10;
const LONG_PRESS_MS = 450;
/** A gesture with no events for this long lost its pointerup (iOS can drop them). */
const STALE_GESTURE_MS = 1500;
/** WHEEL_DELTA units per CSS pixel of two-finger scroll. */
const SCROLL_GAIN = 3;
const WHEEL_GAIN = 1.2;

const BUTTON_LEFT = 1;
const BUTTON_RIGHT = 2;

/** Soft-keyboard keys that arrive without a KeyboardEvent.code. */
const KEY_TO_CODE = {
  Backspace: 'Backspace', Enter: 'Enter', Tab: 'Tab', Escape: 'Escape',
  ArrowUp: 'ArrowUp', ArrowDown: 'ArrowDown', ArrowLeft: 'ArrowLeft', ArrowRight: 'ArrowRight',
};

export class InputController {
  /**
   * @param {{ surface: HTMLElement, textSink: HTMLTextAreaElement, sendBinary: (b: ArrayBuffer) => void, sendJson: (m: object) => void, getVideoSize: () => { width: number, height: number } }} opts
   */
  constructor({ surface, textSink, sendBinary, sendJson, getVideoSize }) {
    this.surface = surface;
    this.textSink = textSink;
    this.sendBinary = sendBinary;
    this.sendJson = sendJson;
    this.getVideoSize = getVideoSize;
    this.mode = 'touch';
    /** pointerId -> { slot, x, y } (last normalised position) */
    this.touches = new Map();
    this.keysDown = new Set();
    this.gesture = null;
    this.rect = null;
  }

  /** 'touch' = native Windows touch; 'mouse' = finger drives the mouse (tap, long-press, two-finger scroll). */
  setMode(mode) {
    this.releaseAll();
    this.mode = mode;
  }

  attach() {
    const s = this.surface;
    const opts = { passive: false };
    s.addEventListener('pointerdown', (e) => this.#onPointer(e, PHASE.DOWN), opts);
    s.addEventListener('pointermove', (e) => this.#onPointer(e, PHASE.MOVE), opts);
    s.addEventListener('pointerup', (e) => this.#onPointer(e, PHASE.UP), opts);
    s.addEventListener('pointercancel', (e) => this.#onPointer(e, PHASE.CANCEL), opts);
    s.addEventListener('pointerleave', (e) => { if (e.pointerType === 'pen' && !e.buttons) this.#onPointer(e, PHASE.CANCEL); }, opts);
    s.addEventListener('wheel', (e) => this.#onWheel(e), opts);
    for (const type of ['contextmenu', 'gesturestart', 'gesturechange', 'touchstart', 'touchmove']) {
      s.addEventListener(type, (e) => e.preventDefault(), opts);
    }
    window.addEventListener('resize', () => { this.rect = null; });
    window.addEventListener('keydown', (e) => this.#onKey(e, true));
    window.addEventListener('keyup', (e) => this.#onKey(e, false));
    // Key-ups and pointer-ups are lost when the iPad switches apps or locks.
    window.addEventListener('blur', () => this.releaseAll());
    document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') this.releaseAll(); });
    this.textSink.addEventListener('input', (e) => this.#onTextInput(e));
  }

  /** Tell the PC to lift every key, finger and button this page is holding. */
  releaseAll() {
    for (const code of this.keysDown) this.sendJson({ t: 'key', code, down: false });
    this.keysDown.clear();
    for (const { slot, x, y } of this.touches.values()) {
      this.sendBinary(encodePointer({ kind: POINTER_KIND.TOUCH, phase: PHASE.CANCEL, id: slot, x, y }));
    }
    this.touches.clear();
    this.#endGesture();
  }

  // ---- geometry ------------------------------------------------------------

  /** Client point -> normalised position inside the letterboxed video. */
  #normalize(clientX, clientY) {
    const r = this.rect ?? (this.rect = this.surface.getBoundingClientRect());
    const { width: vw, height: vh } = this.getVideoSize();
    const scale = Math.min(r.width / vw, r.height / vh);
    const cw = vw * scale;
    const ch = vh * scale;
    return {
      x: (clientX - r.left - (r.width - cw) / 2) / cw,
      y: (clientY - r.top - (r.height - ch) / 2) / ch,
    };
  }

  #send(kind, phase, id, e, buttons = 0) {
    const { x, y } = this.#normalize(e.clientX, e.clientY);
    this.sendBinary(encodePointer({ kind, phase, id, x, y, pressure: e.pressure, tiltX: e.tiltX, tiltY: e.tiltY, buttons }));
    return { x, y };
  }

  // ---- pointers ------------------------------------------------------------

  #onPointer(e, phase) {
    e.preventDefault();
    if (phase === PHASE.DOWN) this.surface.setPointerCapture?.(e.pointerId);
    if (e.pointerType === 'pen') this.#pen(e, phase);
    else if (e.pointerType === 'mouse') this.#send(POINTER_KIND.MOUSE, phase, 0, e, e.buttons);
    else if (this.mode === 'mouse') this.#mouseGesture(e, phase);
    else this.#touch(e, phase);
  }

  #pen(e, phase) {
    const effective = phase === PHASE.MOVE && !(e.buttons & BUTTON_LEFT) ? PHASE.HOVER : phase;
    const samples = phase === PHASE.MOVE && e.getCoalescedEvents ? e.getCoalescedEvents() : [];
    for (const s of samples.length ? samples : [e]) this.#send(POINTER_KIND.PEN, effective, 0, s);
  }

  #touch(e, phase) {
    let entry = this.touches.get(e.pointerId);
    if (phase === PHASE.DOWN && !entry) {
      const used = new Set([...this.touches.values()].map((t) => t.slot));
      const slot = [...Array(MAX_TOUCH_SLOTS).keys()].find((i) => !used.has(i));
      if (slot === undefined) return;
      entry = { slot, x: 0, y: 0 };
      this.touches.set(e.pointerId, entry);
    }
    if (!entry) return;
    Object.assign(entry, this.#send(POINTER_KIND.TOUCH, phase, entry.slot, e));
    if (phase === PHASE.UP || phase === PHASE.CANCEL) this.touches.delete(e.pointerId);
  }

  // ---- "mouse" mode gestures -------------------------------------------------

  #mouse(e, buttons, phase = PHASE.MOVE) {
    this.#send(POINTER_KIND.MOUSE, phase, 0, e, buttons);
  }

  #endGesture() {
    const g = this.gesture;
    if (!g) return;
    clearTimeout(g.timer);
    if (g.state === 'drag') this.#mouse(g.last, 0, PHASE.UP);
    this.gesture = null;
  }

  #mouseGesture(e, phase) {
    if (phase === PHASE.DOWN && this.gesture && performance.now() - this.gesture.updatedAt > STALE_GESTURE_MS) {
      this.#endGesture();
    }
    const g = this.gesture;
    if (g) g.updatedAt = performance.now();

    if (phase === PHASE.DOWN) {
      if (!g) {
        this.gesture = {
          state: 'pending', primary: e.pointerId, start: { x: e.clientX, y: e.clientY }, last: e,
          fingers: new Map([[e.pointerId, { x: e.clientX, y: e.clientY }]]),
          timer: setTimeout(() => this.#longPress(), LONG_PRESS_MS),
          updatedAt: performance.now(),
        };
        this.#mouse(e, 0);
        return;
      }
      g.fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (g.state === 'drag') this.#mouse(g.last, 0, PHASE.UP);
      clearTimeout(g.timer);
      g.state = 'scroll';
      g.centroid = this.#centroid(g.fingers);
      return;
    }
    if (!g || !g.fingers.has(e.pointerId)) return;

    if (phase === PHASE.MOVE) {
      g.fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (g.state === 'scroll') return this.#scroll(g);
      if (e.pointerId !== g.primary) return;
      g.last = e;
      if (g.state === 'pending' && Math.hypot(e.clientX - g.start.x, e.clientY - g.start.y) > TAP_SLOP_PX) {
        clearTimeout(g.timer);
        g.state = 'drag';
        this.#send(POINTER_KIND.MOUSE, PHASE.DOWN, 0, { clientX: g.start.x, clientY: g.start.y }, BUTTON_LEFT);
      }
      if (g.state === 'drag') this.#mouse(e, BUTTON_LEFT);
      return;
    }

    // UP / CANCEL
    g.fingers.delete(e.pointerId);
    if (g.state === 'pending' && phase === PHASE.UP) {
      this.#mouse(e, BUTTON_LEFT, PHASE.DOWN);
      this.#mouse(e, 0, PHASE.UP);
    } else if (g.state === 'drag' && e.pointerId === g.primary) {
      this.#mouse(e, 0, PHASE.UP);
      g.state = 'done';
    }
    if (g.state === 'scroll' && g.fingers.size) g.centroid = this.#centroid(g.fingers);
    if (!g.fingers.size) {
      clearTimeout(g.timer);
      this.gesture = null;
    }
  }

  #longPress() {
    const g = this.gesture;
    if (!g || g.state !== 'pending') return;
    g.state = 'done';
    this.#mouse(g.last, BUTTON_RIGHT, PHASE.DOWN);
    this.#mouse(g.last, 0, PHASE.UP);
  }

  #centroid(fingers) {
    let x = 0;
    let y = 0;
    for (const p of fingers.values()) { x += p.x; y += p.y; }
    return { x: x / fingers.size, y: y / fingers.size };
  }

  #scroll(g) {
    const c = this.#centroid(g.fingers);
    const dx = c.x - g.centroid.x;
    const dy = c.y - g.centroid.y;
    if (Math.abs(dx) + Math.abs(dy) < 1) return;
    g.centroid = c;
    const { x, y } = this.#normalize(c.x, c.y);
    // Natural scrolling: content follows the fingers.
    this.sendBinary(encodeWheel({ x, y, dx: -dx * SCROLL_GAIN, dy: dy * SCROLL_GAIN }));
  }

  // ---- wheel & keyboard ------------------------------------------------------

  #onWheel(e) {
    e.preventDefault();
    const { x, y } = this.#normalize(e.clientX, e.clientY);
    const lineScale = e.deltaMode === 1 ? 40 : 1;
    this.sendBinary(encodeWheel({ x, y, dx: e.deltaX * lineScale * WHEEL_GAIN, dy: -e.deltaY * lineScale * WHEEL_GAIN }));
  }

  #onKey(e, isDown) {
    if (e.isComposing) return;
    const code = e.code || KEY_TO_CODE[e.key];
    if (!code) return; // let the soft keyboard's input event deliver text
    e.preventDefault();
    if (isDown) this.keysDown.add(code);
    else this.keysDown.delete(code);
    this.sendJson({ t: 'key', code, down: isDown });
  }

  #onTextInput(e) {
    if (e.inputType === 'insertText' && e.data) this.sendJson({ t: 'text', s: e.data });
    else if (e.inputType === 'deleteContentBackward') {
      this.sendJson({ t: 'key', code: 'Backspace', down: true });
      this.sendJson({ t: 'key', code: 'Backspace', down: false });
    } else if (e.inputType === 'insertLineBreak') {
      this.sendJson({ t: 'key', code: 'Enter', down: true });
      this.sendJson({ t: 'key', code: 'Enter', down: false });
    }
    this.textSink.value = '';
  }
}
