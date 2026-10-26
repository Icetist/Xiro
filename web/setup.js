// Pairing page (plain HTTP). Detects whether the iPad already trusts the PC's
// certificate and, once it does, hands off to the secure viewer page.
(() => {
  const CHECK_INTERVAL_MS = 2000;
  const HOST_TIMEOUT_MS = 2500;
  const HANDOFF_MS = 700;
  const DEFAULT_PORT = 8443;
  const DOWNLOADED_KEY = 'xiro.profileDownloaded';
  const params = new URLSearchParams(location.hash.slice(1));
  // Only a one-time code: the long-lived token never passes through this plain-HTTP page.
  const code = params.get('c');
  const credential = code ? `c=${encodeURIComponent(code)}` : null;
  const requestedPort = Number(params.get('p'));
  const port = Number.isInteger(requestedPort) && requestedPort > 0 && requestedPort < 65536 ? requestedPort : DEFAULT_PORT;
  const link = document.getElementById('link');
  const show = (id, linkState) => {
    for (const s of document.querySelectorAll('section')) s.hidden = s.id !== id;
    link.dataset.state = linkState;
  };

  const device = /iPhone|iPod/.test(navigator.userAgent) ? 'iPhone' : 'iPad';
  for (const el of document.querySelectorAll('[data-device]')) el.textContent = device;

  if (!credential) {
    show('missing', 'broken');
    return;
  }

  // Step 1 is the only step this page can see happen; the rest happen in Settings.
  const markDownloaded = () => {
    document.getElementById('step-profile').classList.add('done');
    document.body.classList.add('downloaded');
  };
  try { if (sessionStorage.getItem(DOWNLOADED_KEY)) markDownloaded(); } catch { /* private mode */ }
  document.getElementById('download').addEventListener('click', () => {
    markDownloaded();
    try { sessionStorage.setItem(DOWNLOADED_KEY, '1'); } catch { /* private mode */ }
  });

  // The PC's mDNS name (e.g. MYPC.local) keeps working when the router hands the PC a
  // new IP, so a Home Screen icon saved from it never goes stale. The IP is the fallback.
  const name = params.get('h');
  const hosts = [/^[A-Za-z0-9-]{1,63}\.local$/.test(name ?? '') ? name : null, location.hostname].filter(Boolean);
  let timer = null;

  /** First host that resolves and presents a trusted certificate, in order of preference. */
  async function firstReachable() {
    for (const host of hosts) {
      try {
        // An opaque no-cors response is enough: the request only succeeds if TLS is trusted.
        // Bounded, so an unresolvable .local name (mDNS blocked) quickly falls back to the IP.
        await fetch(`https://${host}:${port}/api/ping`, { mode: 'no-cors', cache: 'no-store', signal: AbortSignal.timeout(HOST_TIMEOUT_MS) });
        return host;
      } catch { /* try the next one */ }
    }
    return null;
  }

  async function check() {
    const host = await firstReachable();
    if (host) {
      show('ready', 'linked');
      setTimeout(() => location.replace(`https://${host}:${port}/#${credential}`), HANDOFF_MS);
      return;
    }
    document.getElementById('error').textContent = 'Not trusted yet. Checking again in a moment.';
    show('steps', 'waiting');
    clearTimeout(timer);
    if (document.visibilityState === 'visible') timer = setTimeout(check, CHECK_INTERVAL_MS);
  }

  document.getElementById('retry').addEventListener('click', check);
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') check(); });
  check();
})();
