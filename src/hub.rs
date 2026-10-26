//! Coordinates viewers, display, video and input. One task, so state needs no locks; slow
//! display work runs on a blocking worker, one job at a time, and the newest request wins.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use xiro_video::{EncoderPreference, StreamSpec, clock};

use crate::log;
use crate::protocol::{Screen, Settings};
use crate::streamer::{Streamer, ViewerId};

/// Where the picture comes from: the on-demand virtual monitor, or an existing display.
pub trait Provider: Send + Sync + 'static {
    fn is_virtual(&self) -> bool;
    /// Blocking: may reload the display driver and test encoders.
    fn acquire(&self, screen: Option<Screen>, progress: &dyn Fn(&str), cancel: &AtomicBool) -> Result<Stream, String>;
    /// Blocking: turns the virtual monitor off (a no-op when it already is).
    fn release(&self);
}

pub trait Video: Send + Sync + 'static {
    fn start(&self, spec: StreamSpec);
    fn stop(&self);
    fn set_bitrate(&self, bitrate: u32);
}

pub trait Input: Send + Sync + 'static {
    fn release_all(&self);
    /// Input now maps onto this monitor.
    fn target(&self, gdi_name: &str);
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub encoder: EncoderPreference,
}

pub enum Msg {
    Join(ViewerId),
    Hello(ViewerId, Option<Screen>),
    Screen(ViewerId, Option<Screen>),
    Leave(ViewerId),
    Settings(ViewerId, Settings),
    VideoStarted { encoder: String, hardware: bool },
    VideoError(String),
    Progress(String),
    JobDone(Job, Result<Stream, String>),
    IdleExpired(u64),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Job {
    Show(Option<Screen>),
    Release,
    /// After a failed setup: turn the monitor off even though viewers are still connected.
    Cleanup,
}

pub struct HubConfig {
    pub idle_stop: Duration,
    pub gop_seconds: f64,
    /// Receives a one-line status whenever it changes (shown in the tray).
    pub on_status: Arc<dyn Fn(String) + Send + Sync>,
}

pub struct Hub {
    provider: Arc<dyn Provider>,
    video: Arc<dyn Video>,
    input: Arc<dyn Input>,
    streamer: Arc<Mutex<Streamer>>,
    controller: Arc<AtomicU64>,
    tx: mpsc::UnboundedSender<Msg>,
    config: HubConfig,
    viewers: Vec<(ViewerId, Option<Screen>)>,
    stream: Option<(Stream, Option<Screen>)>,
    encoder_label: String,
    wanted: Option<Job>,
    busy: bool,
    idle_generation: u64,
    cancel: Arc<AtomicBool>,
}

/// No viewer controls input.
pub const NO_CONTROLLER: u64 = 0;

/// A short, readable encoder name for the iPad's menu.
pub fn encoder_label(name: &str, hardware: bool) -> String {
    let short = name.replace(" MFT", "").replace(" H.264 Encoder", "").replace("H264 Encoder", "").trim().to_owned();
    match (hardware, short.is_empty()) {
        (true, false) => format!("{short} (hardware)"),
        (true, true) => "Hardware encoder".into(),
        (false, _) => "Windows software encoder".into(),
    }
}

impl Hub {
    pub fn new(
        provider: Arc<dyn Provider>,
        video: Arc<dyn Video>,
        input: Arc<dyn Input>,
        streamer: Arc<Mutex<Streamer>>,
        config: HubConfig,
    ) -> (Hub, mpsc::UnboundedReceiver<Msg>, mpsc::UnboundedSender<Msg>, Arc<AtomicU64>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let controller = Arc::new(AtomicU64::new(NO_CONTROLLER));
        let hub = Hub {
            provider,
            video,
            input,
            streamer,
            controller: controller.clone(),
            tx: tx.clone(),
            config,
            viewers: Vec::new(),
            stream: None,
            encoder_label: String::new(),
            wanted: None,
            busy: false,
            idle_generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        (hub, rx, tx, controller)
    }

    /// Runs until shutdown; `rx` is the receiver [`Hub::new`] returned.
    pub async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            if let Msg::Shutdown(done) = msg {
                self.shutdown().await;
                let _ = done.send(());
                // Late job results still need handling (a setup finishing after shutdown).
                while self.busy {
                    match rx.recv().await {
                        Some(Msg::JobDone(job, _)) => {
                            self.busy = false;
                            // Even a failed setup may have turned the monitor on before failing.
                            if matches!(job, Job::Show(_)) {
                                self.release_now().await;
                            }
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                return;
            }
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Join(id) => self.on_join(id),
            Msg::Hello(id, screen) => self.on_hello(id, screen),
            Msg::Screen(id, screen) => self.on_screen(id, screen),
            Msg::Leave(id) => self.on_leave(id),
            Msg::Settings(id, settings) => {
                if self.is_controller(id) {
                    self.apply_settings(settings);
                }
            }
            Msg::VideoStarted { encoder, hardware } => {
                self.encoder_label = encoder_label(&encoder, hardware);
                self.broadcast_welcome();
            }
            Msg::VideoError(message) => log::warning!("{message}"),
            Msg::Progress(text) => self.broadcast(json!({ "t": "status", "text": text })),
            Msg::JobDone(job, result) => self.on_job_done(job, result),
            Msg::IdleExpired(generation) => {
                if generation == self.idle_generation && self.viewers.is_empty() {
                    self.wish(Job::Release);
                }
            }
            Msg::Shutdown(_) => {}
        }
    }

    fn is_controller(&self, id: ViewerId) -> bool {
        self.controller.load(Ordering::Relaxed) == id
    }

    pub fn describe(&self) -> Value {
        let display = self.stream.as_ref().map(|(s, _)| json!({ "name": s.name, "width": s.width, "height": s.height }));
        let (fps, bitrate) = self.stream.as_ref().map_or((0, 0), |(s, _)| (s.fps, s.bitrate));
        json!({
            "t": "welcome",
            "display": display,
            "virtual": self.provider.is_virtual(),
            "encoder": self.encoder_label,
            "fps": fps,
            "bitrateMbps": (bitrate as f64 / 1e5).round() / 10.0,
        })
    }

    fn broadcast(&self, msg: Value) {
        if let Ok(streamer) = self.streamer.lock() {
            streamer.broadcast_text(&msg.to_string());
        }
    }

    pub fn status_line(&self) -> String {
        let n = self.viewers.len();
        let who = match n {
            0 => return "Waiting for an iPad or iPhone".to_owned(),
            1 => "1 device connected".to_owned(),
            _ => format!("{n} devices connected"),
        };
        match &self.stream {
            Some((s, _)) => format!("{who} · {}×{} · {} fps", s.width, s.height, s.fps),
            None => format!("{who} · starting…"),
        }
    }

    fn report_status(&self) {
        (self.config.on_status)(self.status_line());
    }

    fn broadcast_welcome(&self) {
        self.broadcast(self.describe());
        self.report_status();
    }

    fn on_join(&mut self, id: ViewerId) {
        self.idle_generation += 1;
        if self.wanted == Some(Job::Release) {
            self.wanted = None;
        }
        // The newest viewer takes control; lift anything the previous one was holding.
        if self.controller.load(Ordering::Relaxed) != NO_CONTROLLER {
            self.input.release_all();
        }
        self.controller.store(id, Ordering::Relaxed);
        self.viewers.push((id, None));
        self.report_status();
    }

    fn set_screen(&mut self, id: ViewerId, screen: Option<Screen>) {
        if let Some(entry) = self.viewers.iter_mut().find(|(v, _)| *v == id) {
            entry.1 = screen;
        }
    }

    fn on_hello(&mut self, id: ViewerId, screen: Option<Screen>) {
        let mut welcome = self.describe();
        welcome["hostTime"] = json!(clock::now_ms());
        if let Ok(streamer) = self.streamer.lock() {
            streamer.send_text(id, &welcome.to_string());
        }
        self.set_screen(id, screen);
        if self.is_controller(id) {
            self.wish(Job::Show(screen));
        }
    }

    fn on_screen(&mut self, id: ViewerId, screen: Option<Screen>) {
        let Some(screen) = screen else { return };
        self.set_screen(id, Some(screen));
        if self.is_controller(id) {
            self.wish(Job::Show(Some(screen)));
        }
    }

    fn on_leave(&mut self, id: ViewerId) {
        self.viewers.retain(|(v, _)| *v != id);
        self.report_status();
        if self.is_controller(id) {
            self.input.release_all();
            let next = self.viewers.last().copied();
            self.controller.store(next.map_or(NO_CONTROLLER, |(v, _)| v), Ordering::Relaxed);
            // The remaining iPad may be a different model: reshape the display for it.
            if let Some((_, Some(screen))) = next {
                self.wish(Job::Show(Some(screen)));
            }
        }
        if self.viewers.is_empty() {
            // Grace period with the encoder warm, so a Wi-Fi blip or app switch recovers instantly.
            self.idle_generation += 1;
            let (generation, tx, delay) = (self.idle_generation, self.tx.clone(), self.config.idle_stop);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(Msg::IdleExpired(generation));
            });
        }
    }

    fn same_screen(&self, screen: Option<Screen>) -> bool {
        match (&self.stream, screen) {
            (Some(_), None) => true,
            (Some((_, current)), Some(s)) => *current == Some(s),
            (None, _) => false,
        }
    }

    fn wish(&mut self, job: Job) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        self.wanted = Some(job);
        self.next_job();
    }

    fn next_job(&mut self) {
        if self.busy {
            return;
        }
        let Some(job) = self.wanted.take() else { return };
        match job {
            Job::Release if !self.viewers.is_empty() => {}
            Job::Show(_) if self.viewers.is_empty() => {}
            // A mirrored display doesn't change shape, so only the first request does any work.
            Job::Show(screen) if self.stream.is_some() && (!self.provider.is_virtual() || self.same_screen(screen)) => {
                if let Some((stream, _)) = &self.stream {
                    self.video.start(self.spec(stream));
                }
            }
            Job::Show(_) => {
                self.broadcast(json!({ "t": "status", "text": if self.provider.is_virtual() { "Connecting the display…" } else { "Starting stream…" } }));
                // Reshaping the display: stop the old stream so it doesn't compete with encoder tests.
                self.video.stop();
                self.stream = None;
                self.spawn_job(job);
            }
            Job::Release | Job::Cleanup => {
                self.stop_stream();
                self.spawn_job(job);
            }
        }
    }

    fn spawn_job(&mut self, job: Job) {
        self.busy = true;
        let (provider, tx, cancel) = (self.provider.clone(), self.tx.clone(), self.cancel.clone());
        tokio::task::spawn_blocking(move || {
            let result = match job {
                Job::Show(screen) => {
                    let progress_tx = tx.clone();
                    let progress = move |text: &str| {
                        let _ = progress_tx.send(Msg::Progress(text.to_owned()));
                    };
                    provider.acquire(screen, &progress, &cancel)
                }
                Job::Release | Job::Cleanup => {
                    provider.release();
                    Err(String::new())
                }
            };
            let _ = tx.send(Msg::JobDone(job, result));
        });
    }

    fn spec(&self, stream: &Stream) -> StreamSpec {
        StreamSpec {
            display: stream.name.clone(),
            width: stream.width,
            height: stream.height,
            fps: stream.fps,
            bitrate: stream.bitrate,
            gop: ((stream.fps as f64 * self.config.gop_seconds).round() as u32).max(1),
            encoder: stream.encoder.clone(),
        }
    }

    fn on_job_done(&mut self, job: Job, result: Result<Stream, String>) {
        self.busy = false;
        if let Job::Show(screen) = job {
            match result {
                Ok(stream) if self.cancel.load(Ordering::Relaxed) => drop(stream),
                Ok(stream) => self.start_stream(stream, screen),
                Err(err) => {
                    log::error!("Could not start the display: {err}");
                    // A half-finished setup may have left the monitor on: never leave windows on an unseen screen.
                    if self.wanted.is_none() {
                        self.wanted = Some(Job::Cleanup);
                    }
                    self.broadcast(json!({ "t": "status", "text": "Could not start the display", "detail": err, "error": true }));
                }
            }
        }
        self.next_job();
    }

    fn start_stream(&mut self, stream: Stream, screen: Option<Screen>) {
        let previous = self.stream.as_ref().and_then(|(_, s)| *s);
        self.input.target(&stream.name);
        if let Ok(mut streamer) = self.streamer.lock() {
            streamer.set_bitrate(stream.bitrate);
        }
        log::info!("Streaming {} {}x{} at {} fps", stream.name, stream.width, stream.height, stream.fps);
        self.video.start(self.spec(&stream));
        self.stream = Some((stream, screen.or(previous)));
        self.broadcast_welcome();
    }

    fn stop_stream(&mut self) {
        self.video.stop();
        self.stream = None;
        self.report_status();
        if let Ok(mut streamer) = self.streamer.lock() {
            streamer.reset();
        }
    }

    fn apply_settings(&mut self, settings: Settings) {
        let Some((stream, screen)) = self.stream.clone() else { return };
        let fps = settings.fps.filter(|f| *f != stream.fps);
        let bitrate = settings.bitrate_mbps.map(|b| (b * 1e6).round() as u32).filter(|b| *b != stream.bitrate);
        if fps.is_none() && bitrate.is_none() {
            return;
        }
        let next = Stream { fps: fps.unwrap_or(stream.fps), bitrate: bitrate.unwrap_or(stream.bitrate), ..stream };
        if let Ok(mut streamer) = self.streamer.lock() {
            streamer.set_bitrate(next.bitrate);
        }
        if fps.is_some() {
            self.video.start(self.spec(&next));
        } else {
            self.video.set_bitrate(next.bitrate);
        }
        log::info!("Settings changed: {} fps, {:.1} Mbit/s", next.fps, next.bitrate as f64 / 1e6);
        self.stream = Some((next, screen));
        self.broadcast_welcome();
    }

    async fn release_now(&self) {
        let provider = self.provider.clone();
        let _ = tokio::task::spawn_blocking(move || provider.release()).await;
    }

    /// On exit: stop encoding, abandon any display setup in progress, give the PC its layout back.
    async fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.idle_generation += 1;
        self.wanted = None;
        self.input.release_all();
        self.stop_stream();
        self.release_now().await;
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
