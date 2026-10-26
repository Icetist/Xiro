//! One connected iPad: control messages, input forwarding, liveness and cleanup.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use xiro_input::{InputHandle, Pointer, Wheel};
use xiro_video::clock;

use crate::hub;
use crate::log;
use crate::protocol::{MSG_POINTER, MSG_WHEEL, parse_screen, parse_settings};
use crate::streamer::{Link, Streamer, ViewerId};

const MAX_KEY_CODE_LENGTH: usize = 32;
const MAX_TEXT_CHARS: usize = 256;
/// The client pings every 2 s; a silent socket (iPad asleep, Wi-Fi gone) is dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(6);
/// Generous for 240 Hz Pencil + 10 fingers, far below anything abusive.
const MESSAGES_PER_SECOND: f64 = 2000.0;
const MESSAGE_BURST: f64 = 4000.0;
const REPLAY_THROTTLE: Duration = Duration::from_millis(500);
const SETTINGS_COOLDOWN: Duration = Duration::from_millis(500);
const MAX_CONTROL_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTROL_MESSAGE_BYTES: usize = 4 * 1024;
const WRITER_FLUSH: Duration = Duration::from_secs(1);
/// Close code for "this device was unlinked at the PC" (4000-4999 are for applications).
const UNLINKED_CLOSE_CODE: u16 = 4001;

pub struct Context {
    pub streamer: Arc<Mutex<Streamer>>,
    pub hub: mpsc::UnboundedSender<hub::Msg>,
    pub controller: Arc<AtomicU64>,
    pub input: InputHandle,
    /// Asks the encoder for a key frame (a viewer needs one and the GOP cache can't help).
    pub force_key: Arc<dyn Fn() + Send + Sync>,
    pub next_id: AtomicU64,
    pub max_viewers: usize,
    /// Told who connected: the newest device takes control of the PC, so the PC user should know.
    pub on_join: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Token bucket: false once the client exceeds its message budget.
pub struct RateLimiter {
    per_second: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(per_second: f64, burst: f64) -> Self {
        Self { per_second, burst, tokens: burst, last: Instant::now() }
    }

    pub fn allow_at(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = self.burst.min(self.tokens + elapsed * self.per_second);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// The socket's outgoing side: a queue drained by the writer task.
pub struct WsLink {
    tx: mpsc::UnboundedSender<Message>,
    queued: Arc<AtomicUsize>,
    open: Arc<AtomicBool>,
}

impl WsLink {
    fn enqueue(&self, message: Message, len: usize) {
        if !self.open.load(Ordering::Relaxed) {
            return;
        }
        self.queued.fetch_add(len, Ordering::Relaxed);
        if self.tx.send(message).is_err() {
            self.open.store(false, Ordering::Relaxed);
        }
    }

    fn close(&self, code: CloseCode, reason: &str) {
        self.enqueue(Message::Close(Some(CloseFrame { code, reason: reason.to_owned().into() })), 0);
        self.open.store(false, Ordering::Relaxed);
    }
}

impl Link for WsLink {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
    fn buffered(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
    fn send_binary(&self, data: Bytes) {
        let len = data.len();
        self.enqueue(Message::Binary(data), len);
    }
    fn send_text(&self, text: String) {
        let len = text.len();
        self.enqueue(Message::Text(text.into()), len);
    }
    fn unlink(&self) {
        // The web client forgets its token and stops reconnecting on exactly this code.
        self.close(CloseCode::Library(UNLINKED_CLOSE_CODE), "Unlinked");
    }
}

struct Session {
    id: ViewerId,
    who: String,
    ctx: Arc<Context>,
    link: Arc<WsLink>,
    last_replay: Option<Instant>,
    last_settings: Option<Instant>,
}

impl Session {
    fn can_control(&self) -> bool {
        self.ctx.controller.load(Ordering::Relaxed) == self.id
    }

    fn reply(&self, msg: Value) {
        if self.link.buffered() > MAX_CONTROL_BUFFER_BYTES {
            self.link.close(CloseCode::Policy, "Not reading");
            return;
        }
        self.link.send_text(msg.to_string());
    }

    fn on_binary(&self, data: &[u8]) {
        if !self.can_control() {
            return;
        }
        match data.first() {
            Some(&MSG_POINTER) => {
                if let Some(p) = Pointer::decode(data) {
                    self.ctx.input.pointer(p);
                }
            }
            Some(&MSG_WHEEL) => {
                if let Some(w) = Wheel::decode(data) {
                    self.ctx.input.wheel(w);
                }
            }
            _ => {}
        }
    }

    fn on_control(&mut self, msg: &Value) {
        let send = |m| {
            let _ = self.ctx.hub.send(m);
        };
        match msg.get("t").and_then(Value::as_str) {
            Some("hello") => send(hub::Msg::Hello(self.id, msg.get("screen").and_then(parse_screen))),
            Some("screen") => send(hub::Msg::Screen(self.id, msg.get("screen").and_then(parse_screen))),
            Some("ping") => {
                if let Some(id) = msg.get("id").and_then(Value::as_f64).filter(|v| v.is_finite()) {
                    self.reply(json!({ "t": "pong", "id": id, "hostTime": clock::now_ms() }));
                }
            }
            Some("needKey") => self.on_need_key(),
            Some("key") => {
                let code = msg.get("code").and_then(Value::as_str).filter(|c| c.len() <= MAX_KEY_CODE_LENGTH);
                if let (true, Some(code)) = (self.can_control(), code) {
                    self.ctx.input.key(code.to_owned(), msg.get("down") == Some(&Value::Bool(true)));
                }
            }
            Some("text") => {
                let text = msg.get("s").and_then(Value::as_str).filter(|s| s.encode_utf16().count() <= MAX_TEXT_CHARS);
                if let (true, Some(text)) = (self.can_control(), text) {
                    self.ctx.input.text(text.to_owned());
                }
            }
            Some("settings") => self.on_settings(msg),
            _ => {}
        }
    }

    fn on_need_key(&mut self) {
        let now = Instant::now();
        if self.last_replay.is_some_and(|t| now - t < REPLAY_THROTTLE) {
            return;
        }
        self.last_replay = Some(now);
        let replayed = self.ctx.streamer.lock().is_ok_and(|mut s| s.replay(self.id));
        if !replayed {
            (self.ctx.force_key)();
        }
    }

    /// Only the controlling iPad changes the stream, and not faster than a person taps.
    fn on_settings(&mut self, msg: &Value) {
        let now = Instant::now();
        if !self.can_control() || self.last_settings.is_some_and(|t| now - t < SETTINGS_COOLDOWN) {
            return;
        }
        let settings = parse_settings(msg);
        if settings.is_empty() {
            return;
        }
        self.last_settings = Some(now);
        let _ = self.ctx.hub.send(hub::Msg::Settings(self.id, settings));
    }
}

/// Runs one viewer until its socket closes. `still_paired` is checked after registering,
/// so an "Unlink all" during the handshake still turns it away.
pub async fn run<S>(ws: WebSocketStream<S>, who: String, ctx: Arc<Context>, still_paired: impl Fn() -> bool)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let (queued, open) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(true)));
    let link = Arc::new(WsLink { tx, queued: queued.clone(), open: open.clone() });
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let len = match &message {
                Message::Binary(b) => b.len(),
                Message::Text(t) => t.len(),
                _ => 0,
            };
            let closing = matches!(message, Message::Close(_));
            let sent = sink.send(message).await;
            queued.fetch_sub(len, Ordering::Relaxed);
            if sent.is_err() || closing {
                break;
            }
        }
        open.store(false, Ordering::Relaxed);
        let _ = sink.close().await;
    });

    let admitted = ctx.streamer.lock().is_ok_and(|mut s| s.try_add(id, link.clone(), ctx.max_viewers));
    if !admitted || !still_paired() {
        if admitted {
            let _ = ctx.streamer.lock().map(|mut s| s.remove(id));
            link.unlink();
        } else {
            link.close(CloseCode::Again, "Too many viewers");
        }
        drop(link);
        let _ = tokio::time::timeout(WRITER_FLUSH, writer).await;
        return;
    }
    log::info!("Viewer connected: {who}");
    (ctx.on_join)(&who);
    let _ = ctx.hub.send(hub::Msg::Join(id));
    let mut session = Session { id, who, ctx: ctx.clone(), link: link.clone(), last_replay: None, last_settings: None };
    let mut limiter = RateLimiter::new(MESSAGES_PER_SECOND, MESSAGE_BURST);

    loop {
        let next = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                log::info!("Viewer {} went silent; disconnecting", session.who);
                break;
            }
        };
        let Some(Ok(message)) = next else { break };
        if !limiter.allow_at(Instant::now()) {
            log::warning!("Viewer {} exceeded the message rate limit; disconnecting", session.who);
            link.close(CloseCode::Policy, "Rate limit");
            break;
        }
        if !link.is_open() {
            break; // unlinked or closed meanwhile: its last message is not acted on
        }
        match message {
            Message::Binary(data) => session.on_binary(&data),
            // Control messages are tiny; big ones are only parsing work for us.
            Message::Text(text) if text.len() > MAX_CONTROL_MESSAGE_BYTES => {}
            Message::Text(text) => {
                if let Ok(msg) = serde_json::from_str::<Value>(&text) {
                    session.on_control(&msg);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
        if !link.is_open() {
            break;
        }
    }

    let stats = ctx.streamer.lock().ok().and_then(|mut s| s.remove(id));
    // Dropping the last sender lets the writer flush what is queued (e.g. a close frame) and end.
    let who = session.who.clone();
    drop(session);
    drop(link);
    if tokio::time::timeout(WRITER_FLUSH, writer).await.is_err() {
        log::debug!("Viewer {who}: writer did not finish in time");
    }
    let stats = stats.unwrap_or_default();
    log::info!("Viewer disconnected: {} (sent {} frames, dropped {})", who, stats.sent, stats.dropped);
    let _ = ctx.hub.send(hub::Msg::Leave(id));
}

#[cfg(test)]
mod tests {
    use super::RateLimiter;
    use std::time::{Duration, Instant};

    #[test]
    fn rate_limiter_allows_bursts_then_refills() {
        let start = Instant::now();
        let mut allow = RateLimiter::new(10.0, 5.0);
        allow.last = start;
        for _ in 0..5 {
            assert!(allow.allow_at(start));
        }
        assert!(!allow.allow_at(start));
        let later = start + Duration::from_millis(500);
        for _ in 0..5 {
            assert!(allow.allow_at(later));
        }
        assert!(!allow.allow_at(later));
    }
}
