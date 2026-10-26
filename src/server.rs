//! HTTPS app + WSS stream server, and a plain-HTTP setup server that only serves
//! the pairing page and the (public) local CA certificate.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

use crate::assets::{self, SETUP_FILES};
use crate::http::{self, MAX_HEAD_BYTES, Request, Response};
use crate::log;
use crate::viewer;

/// Connection slots per port; the setup port gets few so it can't starve the viewer port.
const APP_CONNECTIONS: usize = 48;
const SETUP_CONNECTIONS: usize = 16;
/// Simultaneous connections from one address: a page needs a few, not dozens.
const CONNECTIONS_PER_ADDRESS: usize = 8;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(15);
const TLS_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WS_PAYLOAD: usize = 64 * 1024;
const AUTH_FAILURES_PER_MINUTE: u32 = 20;
const AUTH_WINDOW: Duration = Duration::from_secs(60);
const LIMITER_MAX_ENTRIES: usize = 1024;
/// Longest a pairing request waits for the PC's answer (the dialog itself gives up sooner).
const APPROVAL_WAIT: Duration = Duration::from_secs(75);

const BASE_HEADERS: [(&str, &str); 6] = [
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Cache-Control", "no-cache"),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    ("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
];
const CSP_BASE: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Failed pairing attempts per address; blocks guessing, with bounded memory.
pub struct AuthLimiter {
    failures: HashMap<IpAddr, (u32, Instant)>,
}

impl AuthLimiter {
    pub fn new() -> Self {
        Self { failures: HashMap::new() }
    }

    pub fn blocked(&self, ip: IpAddr, now: Instant) -> bool {
        self.failures.get(&ip).is_some_and(|(count, reset)| *reset > now && *count >= AUTH_FAILURES_PER_MINUTE)
    }

    pub fn fail(&mut self, ip: IpAddr, now: Instant) {
        if self.failures.len() >= LIMITER_MAX_ENTRIES {
            self.failures.retain(|_, (_, reset)| *reset >= now);
        }
        // Still full of live entries (a flood from many addresses): drop the oldest.
        if self.failures.len() >= LIMITER_MAX_ENTRIES && !self.failures.contains_key(&ip) {
            if let Some(oldest) = self.failures.iter().min_by_key(|(_, (_, reset))| *reset).map(|(ip, _)| *ip) {
                self.failures.remove(&oldest);
            }
        }
        let entry = self.failures.entry(ip).or_insert((0, now + AUTH_WINDOW));
        if entry.1 < now {
            *entry = (0, now + AUTH_WINDOW);
        }
        entry.0 += 1;
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.failures.len()
    }
}

pub struct Shared {
    /// Replaced when the person at the PC unlinks every device.
    pub token: Arc<crate::config::PairingToken>,
    pub codes: Arc<crate::pairing::PairingCodes>,
    pub ca_der: Vec<u8>,
    pub https_port: u16,
    pub viewer: Arc<viewer::Context>,
    pub limiter: Mutex<AuthLimiter>,
    /// Asks the person at the PC whether the device at this address may pair (blocking).
    /// Only reached with a valid one-time code, so it can't be used to spam the PC.
    pub approve: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

fn with_base(mut response: Response, csp: &str) -> Response {
    for (name, value) in BASE_HEADERS {
        response = response.header(name, value);
    }
    response.header("Content-Security-Policy", csp.to_owned())
}

fn file_response(asset: &assets::Asset, csp: &str) -> Response {
    with_base(Response::new(200, asset.body.to_vec()).header("Content-Type", asset.mime), csp)
}

fn not_found() -> Response {
    with_base(Response::new(404, b"Not found".to_vec()).header("Content-Type", "text/plain"), "default-src 'none'")
}

/// Reads one request head (the first may take longer: TLS and the page are being set up).
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S, buf: &mut Vec<u8>, timeout: Duration) -> Option<Request> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(end) = http::head_end(buf) {
            let request = http::parse(&buf[..end]);
            buf.drain(..end);
            return request;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return None;
        }
        let mut chunk = [0u8; 2048];
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, response: Response, keep_alive: bool) -> bool {
    stream.write_all(&response.to_bytes(keep_alive)).await.is_ok() && stream.flush().await.is_ok()
}

enum Route {
    Respond(Response),
    /// The handshake answer, and the pairing token the device used.
    Upgrade(String, String),
    /// A valid one-time code: hand out the token once the person at the PC agrees.
    Approve,
}

fn app_route(req: &Request, shared: &Shared, ip: IpAddr) -> Route {
    if req.path == "/ws" {
        let now = Instant::now();
        let Ok(mut limiter) = shared.limiter.lock() else { return Route::Respond(not_found()) };
        if limiter.blocked(ip, now) {
            return Route::Respond(Response::new(401, Vec::new()));
        }
        let authorised = http::is_same_origin(req) && shared.token.matches(req.query.get("token").map(String::as_str));
        if !req.wants_websocket() || !authorised {
            limiter.fail(ip, now);
            log::warning!("Rejected connection from {ip} (bad pairing token or origin)");
            return Route::Respond(Response::new(401, Vec::new()));
        }
        return match req.header("sec-websocket-key") {
            Some(key) => Route::Upgrade(derive_accept_key(key.as_bytes()), req.query.get("token").cloned().unwrap_or_default()),
            None => Route::Respond(Response::new(400, Vec::new())),
        };
    }
    if req.path == "/api/pair" {
        return redeem(req, shared, ip);
    }
    if req.path == "/api/ping" {
        // Probed with mode:'no-cors' by the setup page; no CORS grant, so other sites learn nothing.
        let mut response = with_base(Response::new(204, Vec::new()), "default-src 'none'");
        response.headers.retain(|(name, _)| *name != "Cross-Origin-Resource-Policy");
        return Route::Respond(response.header("Cross-Origin-Resource-Policy", "cross-origin"));
    }
    let path = if req.path == "/" { "/index.html" } else { req.path.as_str() };
    let csp = format!("{CSP_BASE}; connect-src 'self'");
    Route::Respond(assets::find(path).map_or_else(not_found, |a| file_response(a, &csp)))
}

/// Checks a one-time pairing code (HTTPS only; failures count against the address). A code
/// is used up even if refused at the PC, so a photographed code can't be retried.
fn redeem(req: &Request, shared: &Shared, ip: IpAddr) -> Route {
    let now = Instant::now();
    let Ok(mut limiter) = shared.limiter.lock() else { return Route::Respond(not_found()) };
    let code = req.query.get("code").map(String::as_str).unwrap_or_default();
    if limiter.blocked(ip, now) || !shared.codes.redeem(code) {
        limiter.fail(ip, now);
        log::warning!("Rejected a pairing code from {ip}");
        return Route::Respond(with_base(Response::new(401, Vec::new()), "default-src 'none'"));
    }
    Route::Approve
}

/// The answer to an approved (or refused) pairing request.
fn pairing_answer(shared: &Shared, ip: IpAddr, approved: bool) -> Response {
    if !approved {
        log::warning!("Pairing refused at the PC for {ip}");
        return with_base(Response::new(403, Vec::new()), "default-src 'none'");
    }
    log::info!("Paired a new device at {ip}");
    let body = serde_json::json!({ "token": shared.token.get() }).to_string().into_bytes();
    with_base(Response::new(200, body), "default-src 'none'").header("Content-Type", "application/json").header("Cache-Control", "no-store")
}

async fn ask_at_pc(shared: &Arc<Shared>, ip: IpAddr) -> Response {
    let (asker, who) = (shared.approve.clone(), ip.to_string());
    let asked = tokio::task::spawn_blocking(move || asker(&who));
    // The dialog says no by itself after its timeout; this only bounds a dialog that never returns.
    let approved = matches!(tokio::time::timeout(APPROVAL_WAIT, asked).await, Ok(Ok(true)));
    pairing_answer(shared, ip, approved)
}

fn setup_route(req: &Request, shared: &Shared) -> Response {
    if req.path == "/xiro-ca.crt" {
        return with_base(Response::new(200, shared.ca_der.clone()), "default-src 'none'")
            .header("Content-Type", "application/x-x509-ca-cert")
            .header("Content-Disposition", "attachment; filename=\"Xiro-Local-CA.crt\"");
    }
    let path = if req.path == "/" { "/setup.html" } else { req.path.as_str() };
    // The setup page only needs to probe the secure port on the same machine.
    let csp = format!("{CSP_BASE}; connect-src https://*:{}", shared.https_port);
    match SETUP_FILES.contains(&path).then(|| assets::find(path)).flatten() {
        Some(asset) => file_response(asset, &csp),
        None => not_found(),
    }
}

async fn serve<S>(mut stream: S, peer: SocketAddr, shared: Arc<Shared>, secure: bool)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut buf = Vec::new();
    let mut timeout = HEAD_TIMEOUT;
    while let Some(req) = read_head(&mut stream, &mut buf, timeout).await {
        timeout = KEEP_ALIVE_IDLE;
        let keep_alive = req.keep_alive();
        // We never read bodies: a request announcing one would desynchronise keep-alive.
        let has_body = req.header("transfer-encoding").is_some() || req.header("content-length").is_some_and(|v| v.trim() != "0");
        if req.method != "GET" || has_body {
            let _ = send(&mut stream, with_base(Response::new(405, Vec::new()), "default-src 'none'"), false).await;
            return;
        }
        let route = match if secure { app_route(&req, &shared, peer.ip()) } else { Route::Respond(setup_route(&req, &shared)) } {
            Route::Approve => Route::Respond(ask_at_pc(&shared, peer.ip()).await),
            other => other,
        };
        match route {
            Route::Respond(response) => {
                let close = response.status == 401 || !keep_alive;
                if !send(&mut stream, response, !close).await || close {
                    return;
                }
            }
            Route::Upgrade(accept, token) => {
                let response = Response::new(101, Vec::new())
                    .header("Upgrade", "websocket")
                    .header("Connection", "Upgrade")
                    .header("Sec-WebSocket-Accept", accept);
                if !send(&mut stream, response, true).await {
                    return;
                }
                let config = WebSocketConfig::default().max_message_size(Some(MAX_WS_PAYLOAD)).max_frame_size(Some(MAX_WS_PAYLOAD));
                let ws = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
                // Re-checked after registering: "Unlink all" may have run during the handshake.
                let current = shared.token.clone();
                let still_paired = move || current.matches(Some(&token));
                viewer::run(ws, peer.ip().to_string(), shared.viewer.clone(), still_paired).await;
                return;
            }
            Route::Approve => return, // answered above
        }
    }
}

async fn bind(port: u16, label: &str) -> Result<TcpListener, String> {
    TcpListener::bind(("0.0.0.0", port)).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::AddrInUse => format!("{label} port {port} is already in use. Is Xiro already running (look for its icon in the taskbar tray)?"),
        _ => format!("{label} port {port}: {err}"),
    })
}

/// Connections currently open per address, so one host can't take every slot.
#[derive(Default)]
pub struct PerAddress {
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// Holds one of an address's slots; frees it when the connection ends.
pub struct AddressSlot {
    owner: Arc<PerAddress>,
    ip: IpAddr,
}

impl PerAddress {
    pub fn claim(self: &Arc<Self>, ip: IpAddr) -> Option<AddressSlot> {
        let mut open = self.open.lock().ok()?;
        let count = open.entry(ip).or_insert(0);
        if *count >= CONNECTIONS_PER_ADDRESS {
            return None;
        }
        *count += 1;
        Some(AddressSlot { owner: self.clone(), ip })
    }
}

impl Drop for AddressSlot {
    fn drop(&mut self) {
        if let Ok(mut open) = self.owner.open.lock() {
            if let Some(count) = open.get_mut(&self.ip) {
                *count -= 1;
                if *count == 0 {
                    open.remove(&self.ip);
                }
            }
        }
    }
}

/// Binds both ports and serves forever. `tls` is looked up per connection because the
/// certificate changes when the PC changes networks.
pub async fn start<F>(shared: Arc<Shared>, tls: F, http_port: u16) -> Result<(), String>
where
    F: Fn() -> Arc<tokio_native_tls::TlsAcceptor> + Send + Sync + 'static,
{
    let app = bind(shared.https_port, "HTTPS").await?;
    let setup = bind(http_port, "HTTP").await?;
    let per_address = Arc::new(PerAddress::default());

    let (app_shared, app_slots, app_addresses) = (shared.clone(), Arc::new(Semaphore::new(APP_CONNECTIONS)), per_address.clone());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, peer)) = app.accept().await else { continue };
            let Ok(permit) = app_slots.clone().try_acquire_owned() else { continue };
            let Some(slot) = app_addresses.claim(peer.ip()) else { continue };
            let _ = tcp.set_nodelay(true);
            let (tls, shared) = (tls(), app_shared.clone());
            tokio::spawn(async move {
                let _held = (permit, slot);
                if let Ok(Ok(stream)) = tokio::time::timeout(TLS_TIMEOUT, tls.accept(tcp)).await {
                    serve(stream, peer, shared, true).await;
                }
            });
        }
    });
    let setup_slots = Arc::new(Semaphore::new(SETUP_CONNECTIONS));
    tokio::spawn(async move {
        loop {
            let Ok((tcp, peer)) = setup.accept().await else { continue };
            let Ok(permit) = setup_slots.clone().try_acquire_owned() else { continue };
            let Some(slot) = per_address.claim(peer.ip()) else { continue };
            let shared = shared.clone();
            tokio::spawn(async move {
                let _held = (permit, slot);
                serve(tcp, peer, shared, false).await;
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdefghijklmn";

    fn shared(dir: &std::path::Path) -> Shared {
        let (hub, _) = tokio::sync::mpsc::unbounded_channel();
        let viewer = Arc::new(viewer::Context {
            streamer: Arc::new(Mutex::new(crate::streamer::Streamer::new(1))),
            hub,
            controller: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            input: xiro_input::InputHandle::spawn(false, |_| {}),
            force_key: Arc::new(|| {}),
            next_id: std::sync::atomic::AtomicU64::new(1),
            max_viewers: 4,
            on_join: Arc::new(|_| {}),
        });
        Shared {
            token: Arc::new(crate::config::PairingToken::fixed(TOKEN)),
            codes: Arc::new(crate::pairing::PairingCodes::new(dir.join("codes.json"))),
            ca_der: vec![0x30, 0x82],
            https_port: 8443,
            viewer,
            limiter: Mutex::new(AuthLimiter::new()),
            approve: Arc::new(|_| true),
        }
    }

    fn get(target: &str, extra: &str) -> Request {
        http::parse(format!("GET {target} HTTP/1.1\r\nHost: pc:8443\r\n{extra}\r\n").as_bytes()).unwrap()
    }

    fn status(route: Route) -> u16 {
        match route {
            Route::Respond(r) => r.status,
            Route::Upgrade(..) => 101,
            Route::Approve => 0,
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xiro-server-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const UPGRADE: &str = "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n";

    #[test]
    fn the_socket_needs_the_token_and_our_own_origin() {
        let s = shared(&temp("ws"));
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        let good = format!("{UPGRADE}Origin: https://pc:8443\r\n");
        assert_eq!(status(app_route(&get(&format!("/ws?token={TOKEN}"), &good), &s, ip)), 101);
        assert_eq!(status(app_route(&get("/ws?token=wrong", &good), &s, ip)), 401);
        let cross = format!("{UPGRADE}Origin: https://evil.example\r\n");
        assert_eq!(status(app_route(&get(&format!("/ws?token={TOKEN}"), &cross), &s, ip)), 401);
        assert_eq!(s.limiter.lock().unwrap().len(), 1, "failures are counted per address");
    }

    #[test]
    fn a_blocked_address_is_refused_before_any_token_check() {
        let s = shared(&temp("blocked"));
        let ip: IpAddr = "192.168.1.21".parse().unwrap();
        for _ in 0..AUTH_FAILURES_PER_MINUTE {
            s.limiter.lock().unwrap().fail(ip, Instant::now());
        }
        let good = format!("{UPGRADE}Origin: https://pc:8443\r\n");
        assert_eq!(status(app_route(&get(&format!("/ws?token={TOKEN}"), &good), &s, ip)), 401);
    }

    #[test]
    fn a_pairing_code_buys_the_token_once() {
        let s = shared(&temp("pair"));
        let ip: IpAddr = "192.168.1.22".parse().unwrap();
        let code = s.codes.issue().unwrap();
        assert!(matches!(app_route(&get(&format!("/api/pair?code={code}"), ""), &s, ip), Route::Approve), "asks the PC");
        assert_eq!(status(app_route(&get(&format!("/api/pair?code={code}"), ""), &s, ip)), 401, "codes work once");
        assert_eq!(status(app_route(&get("/api/pair", ""), &s, ip)), 401);
    }

    #[test]
    fn the_token_is_only_handed_out_when_the_pc_agrees() {
        let s = shared(&temp("approve"));
        let ip: IpAddr = "192.168.1.22".parse().unwrap();
        let yes = pairing_answer(&s, ip, true);
        assert_eq!(yes.status, 200);
        assert!(String::from_utf8(yes.body).unwrap().contains(TOKEN));
        let no = pairing_answer(&s, ip, false);
        assert_eq!(no.status, 403);
        assert!(!String::from_utf8(no.body).unwrap().contains(TOKEN));
    }

    #[test]
    fn only_embedded_files_are_served() {
        let s = shared(&temp("files"));
        let ip: IpAddr = "192.168.1.23".parse().unwrap();
        assert_eq!(status(app_route(&get("/", ""), &s, ip)), 200);
        assert_eq!(status(app_route(&get("/../Cargo.toml", ""), &s, ip)), 404);
        assert_eq!(status(app_route(&get("/api/ping", ""), &s, ip)), 204);
        assert_eq!(setup_route(&get("/", ""), &s).status, 200);
        assert_eq!(setup_route(&get("/xiro-ca.crt", ""), &s).body, vec![0x30, 0x82]);
        assert_eq!(setup_route(&get("/app.js", ""), &s).status, 404, "the plain-HTTP port serves only the setup page");
    }

    #[test]
    fn one_address_cannot_take_every_connection() {
        let slots = Arc::new(PerAddress::default());
        let ip: IpAddr = "192.168.1.24".parse().unwrap();
        let held: Vec<_> = (0..CONNECTIONS_PER_ADDRESS).map(|_| slots.claim(ip).unwrap()).collect();
        assert!(slots.claim(ip).is_none());
        assert!(slots.claim("192.168.1.25".parse().unwrap()).is_some(), "others still get in");
        drop(held);
        assert!(slots.claim(ip).is_some(), "slots come back when connections end");
    }

    #[test]
    fn limiter_blocks_after_repeated_failures_and_forgets_later() {
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let now = Instant::now();
        let mut limiter = AuthLimiter::new();
        for _ in 0..AUTH_FAILURES_PER_MINUTE {
            assert!(!limiter.blocked(ip, now));
            limiter.fail(ip, now);
        }
        assert!(limiter.blocked(ip, now));
        assert!(!limiter.blocked(ip, now + AUTH_WINDOW + Duration::from_secs(1)));
        limiter.fail(ip, now + AUTH_WINDOW + Duration::from_secs(1));
        assert!(!limiter.blocked(ip, now + AUTH_WINDOW + Duration::from_secs(1)), "a new window starts over");
    }

    #[test]
    fn limiter_memory_stays_bounded_under_a_flood() {
        let now = Instant::now();
        let mut limiter = AuthLimiter::new();
        for i in 0..(LIMITER_MAX_ENTRIES as u32 + 50) {
            limiter.fail(IpAddr::from(i.to_be_bytes()), now + Duration::from_millis(i as u64));
        }
        assert_eq!(limiter.len(), LIMITER_MAX_ENTRIES);
    }
}
