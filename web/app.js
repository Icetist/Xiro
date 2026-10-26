// Xiro viewer: connects to the PC, shows the stream, forwards input.

import { InputController } from './input.js';
import { MSG } from './protocol.js';
import { VideoPipeline } from './video.js';

const PING_INTERVAL_MS = 2000;
const CLOCK_SAMPLES = 8;
const RECONNECT_DELAYS_MS = [300, 600, 1200, 2500, 5000];
const PAIRING_FAILURE_THRESHOLD = 4;
const MENU_AUTOHIDE_MS = 8000;
/** Close code the PC sends when its user clicks "Unlink all devices". */
const CLOSE_UNLINKED = 4001;
const TIP_MS = 9000;
const TIP_MAX_SHOWS = 3;

const $ = (id) => document.getElementById(id);
const els = {
  canvas: $('screen'), kbd: $('kbd'), status: $('status'), statusLink: $('status-link'), statusText: $('status-text'), statusDetail: $('status-detail'),
  stats: $('stats'), tip: $('tip'), handle: $('handle'), menu: $('menu'), menuInfo: $('menu-info'), menuHint: $('menu-hint'),
};

const prefs = {
  get(key, fallback) {
    try { return localStorage.getItem(`xiro.${key}`) ?? fallback; } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(`xiro.${key}`, value); } catch { /* private mode: not persisted */ }
  },
};

const hostClock = { samples: [], offset: 0 };
const nowMs = () => performance.timeOrigin + performance.now();
const toHostTime = (localMs) => localMs + hostClock.offset;

const state = {
  ws: null, attempts: 0, failuresBeforeOpen: 0, pingTimer: null, wakeLock: null, info: null, hz: 60,
  video: { width: 16, height: 9 }, latency: [], decodeMs: [], lastStatsAt: performance.now(), lastBytes: 0, lastDecoded: 0,
};

// ---- pairing token ------------------------------------------------------------

/** Set when the person at the PC said no to pairing this device. */
let pairingRefused = false;

/** Trades the QR link's one-time code for the long-lived token, over HTTPS (the PC asks first). */
async function redeemCode(code) {
  try {
    const res = await fetch(`/api/pair?code=${encodeURIComponent(code)}`, { cache: 'no-store' });
    pairingRefused = res.status === 403;
    if (!res.ok) return null;
    const { token } = await res.json();
    return typeof token === 'string' ? token : null;
  } catch {
    return null;
  }
}

async function readToken() {
  const params = new URLSearchParams(location.hash.slice(1));
  const code = params.get('c');
  if (code && !params.get('t')) showStatus('Allow on your PC', 'Xiro is asking on the PC whether this device may connect. Choose Allow there.', { link: 'waiting' });
  const fromHash = params.get('t') ?? (code ? await redeemCode(code) : null);
  const token = fromHash ?? prefs.get('token', null);
  if (fromHash) prefs.set('token', fromHash);
  // Token in the URL so "Add to Home Screen" bookmarks a working link, never a used one-time code.
  if (token) history.replaceState(null, '', `#t=${encodeURIComponent(token)}`);
  return token;
}

// ---- status overlay -------------------------------------------------------------

/**
 * @param {string} text
 * @param {string} [detail]
 * @param {{ spinner?: boolean, link?: 'searching' | 'waiting' | 'linked' | 'broken' }} [options]
 *   `link` sets the two-screens picture; by default it searches, or shows a gap when `spinner` is false.
 */
function showStatus(text, detail = '', { spinner = true, link } = {}) {
  els.statusText.textContent = text;
  els.statusDetail.textContent = detail;
  els.statusLink.dataset.state = link ?? (spinner ? 'searching' : 'broken');
  els.status.hidden = false;
}

const hideStatus = () => { els.status.hidden = true; };

// ---- video ----------------------------------------------------------------------

const video = new VideoPipeline(els.canvas, {
  onFrame: ({ sentAt, decodeMs, width, height }) => {
    if (!els.status.hidden) hideStatus();
    state.video = { width, height };
    state.latency.push(toHostTime(nowMs()) - sentAt);
    state.decodeMs.push(decodeMs);
  },
  onError: (msg) => showStatus('Video problem', msg),
  onNeedKey: () => sendJson({ t: 'needKey' }),
});

// ---- connection -----------------------------------------------------------------

function send(data) {
  if (state.ws?.readyState === WebSocket.OPEN) state.ws.send(data);
}
const sendJson = (msg) => send(JSON.stringify(msg));

function ping() {
  sendJson({ t: 'ping', id: nowMs() });
}

function onPong({ id, hostTime }) {
  const received = nowMs();
  const rtt = received - id;
  hostClock.samples.push({ rtt, offset: hostTime - (id + received) / 2 });
  if (hostClock.samples.length > CLOCK_SAMPLES) hostClock.samples.shift();
  // The lowest-RTT sample has the least queuing error.
  hostClock.offset = hostClock.samples.reduce((a, b) => (b.rtt < a.rtt ? b : a)).offset;
  state.rtt = rtt;
}

function onWelcome(info) {
  state.info = info;
  if (!info.display) return; // the PC is still preparing the display
  const kind = info.virtual ? 'Second display' : 'Mirroring';
  els.menuInfo.textContent = `${kind} · ${info.display.width}×${info.display.height} · ${info.fps} fps · ${info.bitrateMbps} Mbit/s · ${info.encoder}`;
  // In Sidecar mode the PC shapes the display itself; only a mirrored screen can mismatch.
  const panel = describeScreen();
  const matches = info.virtual || (info.display.width === panel.width && info.display.height === panel.height);
  els.menuHint.hidden = matches;
  els.menuHint.textContent = matches ? '' : 'Mirroring the PC’s screen. Set up the second screen in Xiro on the PC for a display shaped like this device.';
  markSelected('fps', String(info.fps));
  markSelected('bitrateMbps', String(info.bitrateMbps));
}

function onText(raw) {
  let msg;
  try { msg = JSON.parse(raw); } catch { return; }
  if (msg.t === 'config') video.configure(msg);
  else if (msg.t === 'pong') onPong(msg);
  else if (msg.t === 'welcome') onWelcome(msg);
  else if (msg.t === 'status' && typeof msg.text === 'string') {
    showStatus(msg.text, typeof msg.detail === 'string' ? msg.detail : '', { spinner: !msg.error });
  }
}

// ---- screen description -----------------------------------------------------------

const REFRESH_SAMPLES = 24;
const REFRESH_TIMEOUT_MS = 1200;

/** The display's refresh rate, from animation-frame spacing (120 on ProMotion iPads). */
function measureRefreshRate() {
  return new Promise((resolve) => {
    const stamps = [];
    const timer = setTimeout(() => resolve(60), REFRESH_TIMEOUT_MS);
    const tick = (t) => {
      stamps.push(t);
      if (stamps.length <= REFRESH_SAMPLES) {
        requestAnimationFrame(tick);
        return;
      }
      clearTimeout(timer);
      const gaps = stamps.slice(1).map((s, i) => s - stamps[i]);
      resolve(Math.round(1000 / median(gaps)));
    };
    requestAnimationFrame(tick);
  });
}

const landscape = matchMedia('(orientation: landscape)');
const deviceName = /iPhone|iPod/.test(navigator.userAgent) ? 'iPhone' : 'iPad';

/** Native panel pixels in the current orientation: the PC shapes its virtual monitor to match. */
function describeScreen() {
  const long = Math.round(Math.max(screen.width, screen.height) * devicePixelRatio);
  const short = Math.round(Math.min(screen.width, screen.height) * devicePixelRatio);
  return landscape.matches
    ? { width: long, height: short, hz: state.hz }
    : { width: short, height: long, hz: state.hz };
}

function connect(token) {
  const ws = new WebSocket(`wss://${location.host}/ws?token=${encodeURIComponent(token)}`);
  ws.binaryType = 'arraybuffer';
  state.ws = ws;
  let opened = false;

  ws.onopen = () => {
    opened = true;
    state.attempts = 0;
    state.failuresBeforeOpen = 0;
    showStatus('Starting the picture', '', { link: 'linked' });
    sendJson({ t: 'hello', screen: describeScreen() });
    ping();
    clearInterval(state.pingTimer);
    state.pingTimer = setInterval(ping, PING_INTERVAL_MS);
    requestWakeLock();
  };
  ws.onmessage = (e) => {
    if (typeof e.data === 'string') onText(e.data);
    else if (new Uint8Array(e.data, 0, 1)[0] === MSG.VIDEO) video.push(e.data);
  };
  ws.onclose = (e) => {
    clearInterval(state.pingTimer);
    if (e.code === CLOSE_UNLINKED) {
      onUnlinked();
      return;
    }
    if (!opened) state.failuresBeforeOpen += 1;
    if (state.failuresBeforeOpen >= PAIRING_FAILURE_THRESHOLD) {
      showStatus('Can’t reach your PC', 'Is Xiro running there? If it is, scan its code again to reconnect.', { spinner: false });
    } else {
      showStatus('Reconnecting', 'Waiting for your PC');
    }
    const delay = RECONNECT_DELAYS_MS[Math.min(state.attempts, RECONNECT_DELAYS_MS.length - 1)];
    state.attempts += 1;
    setTimeout(() => connect(token), delay);
  };
}

/** The PC forgot this device: drop its key and stay put until a new code is scanned. */
function onUnlinked() {
  try { localStorage.removeItem('xiro.token'); } catch { /* private mode: nothing stored */ }
  history.replaceState(null, '', location.pathname);
  showStatus(`This ${deviceName} was unlinked`, 'Scan the code on your PC to connect again.', { spinner: false });
}

// ---- wake lock / fullscreen -------------------------------------------------------

async function requestWakeLock() {
  try {
    if ('wakeLock' in navigator && !state.wakeLock) {
      state.wakeLock = await navigator.wakeLock.request('screen');
      state.wakeLock.addEventListener('release', () => { state.wakeLock = null; });
    }
  } catch { /* not granted; screen may dim */ }
}

/** Launched from the Home Screen: Safari's bars are already gone. */
const isHomeScreenApp = () => navigator.standalone === true
  || matchMedia('(display-mode: fullscreen), (display-mode: standalone)').matches;
const fullscreenElement = () => document.fullscreenElement ?? document.webkitFullscreenElement;

function setFullscreen(on) {
  const doc = document;
  const root = doc.documentElement;
  const call = on ? (root.requestFullscreen ?? root.webkitRequestFullscreen)?.bind(root)
    : (doc.exitFullscreen ?? doc.webkitExitFullscreen)?.bind(doc);
  // The prefixed API returns nothing; the standard one returns a promise that may reject.
  try { Promise.resolve(call?.()).catch(() => {}); } catch { /* not allowed right now */ }
}

const toggleFullscreen = () => setFullscreen(!fullscreenElement());

function showTip(text) {
  els.tip.textContent = text;
  els.tip.hidden = false;
  const hide = () => { els.tip.hidden = true; };
  els.tip.addEventListener('pointerdown', hide, { once: true });
  setTimeout(hide, TIP_MS);
}

// Fullscreen needs a user gesture, so go edge-to-edge on the first touch and suggest
// the Home Screen app, which hides Safari's bars for good.
function setupImmersive() {
  if (isHomeScreenApp()) return;
  document.addEventListener('touchend', () => {
    if (!fullscreenElement()) setFullscreen(true);
  }, { once: true });

  const shows = Number(prefs.get('tipShows', '0'));
  if (shows >= TIP_MAX_SHOWS) return;
  prefs.set('tipShows', String(shows + 1));
  showTip('For a full-screen app with no Safari bars: tap Share → Add to Home Screen.');
}

// ---- stats ----------------------------------------------------------------------

function median(values) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[sorted.length >> 1];
}

function updateStats() {
  const now = performance.now();
  const seconds = (now - state.lastStatsAt) / 1000;
  const { bytes, decoded, skipped } = video.stats;
  const fps = (decoded - state.lastDecoded) / seconds;
  const mbps = ((bytes - state.lastBytes) * 8) / seconds / 1e6;
  els.stats.textContent = [
    `${fps.toFixed(0)} fps · ${mbps.toFixed(1)} Mbit/s`,
    `latency ${median(state.latency).toFixed(0)} ms (network + decode)`,
    `decode ${median(state.decodeMs).toFixed(1)} ms · rtt ${(state.rtt ?? 0).toFixed(0)} ms`,
    `skipped ${skipped}`,
  ].join('\n');
  Object.assign(state, { lastStatsAt: now, lastBytes: bytes, lastDecoded: decoded, latency: [], decodeMs: [] });
}

// ---- menu -----------------------------------------------------------------------

let menuTimer = null;

function setMenu(open) {
  els.menu.hidden = !open;
  els.handle.setAttribute('aria-expanded', String(open));
  clearTimeout(menuTimer);
  if (open) menuTimer = setTimeout(() => setMenu(false), MENU_AUTOHIDE_MS);
}

function markSelected(setting, value) {
  for (const b of els.menu.querySelectorAll(`[data-setting="${setting}"] button`)) {
    b.classList.toggle('selected', b.dataset.value === value);
  }
}

function setupMenu(input) {
  els.handle.addEventListener('click', () => setMenu(els.menu.hidden));
  els.menu.addEventListener('pointerdown', () => setMenu(true));

  els.menu.addEventListener('click', (e) => {
    const button = e.target.closest('button');
    if (!button) return;
    const action = button.dataset.action;
    if (action === 'fullscreen') toggleFullscreen();
    else if (action === 'keyboard') {
      els.kbd.focus();
      setMenu(false);
    } else if (action === 'stats') {
      els.stats.hidden = !els.stats.hidden;
      prefs.set('stats', els.stats.hidden ? '0' : '1');
    }

    const setting = button.parentElement?.dataset.setting;
    if (!setting) return;
    markSelected(setting, button.dataset.value);
    if (setting === 'mode') {
      input.setMode(button.dataset.value);
      prefs.set('mode', button.dataset.value);
    } else {
      sendJson({ t: 'settings', [setting]: Number(button.dataset.value) });
    }
  });
}

// ---- boot -----------------------------------------------------------------------

async function boot() {
  for (const el of document.querySelectorAll('[data-device]')) el.textContent = deviceName;
  if (!window.isSecureContext || !VideoPipeline.isSupported()) {
    showStatus('Safari needs an update', 'Xiro needs iOS or iPadOS 16.4 or newer. Update in Settings › General › Software Update.', { spinner: false });
    return;
  }
  const token = await readToken();
  if (!token && pairingRefused) {
    showStatus('Not allowed', 'Someone at the PC chose Don’t Allow. Scan a new code and choose Allow on the PC.', { spinner: false });
    return;
  }
  if (!token) {
    showStatus(`Connect this ${deviceName}`, 'Click the Xiro icon in your PC’s taskbar and scan the code it shows. Each code works once, for 15 minutes.', { spinner: false });
    return;
  }

  const input = new InputController({
    surface: els.canvas,
    textSink: els.kbd,
    sendBinary: send,
    sendJson,
    getVideoSize: () => state.video,
  });
  input.attach();
  const mode = prefs.get('mode', 'touch');
  input.setMode(mode);
  markSelected('mode', mode);
  els.stats.hidden = prefs.get('stats', '0') !== '1';

  setupMenu(input);
  setupImmersive();
  setInterval(updateStats, 1000);
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') requestWakeLock(); });
  // Rotating the iPad rotates the PC's virtual monitor, like Sidecar.
  landscape.addEventListener('change', () => sendJson({ t: 'screen', screen: describeScreen() }));
  measureRefreshRate().then((hz) => {
    state.hz = hz;
    connect(token);
  });
}

boot();
