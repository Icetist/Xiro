//! The video thread: capture -> convert -> encode. Commands are plain data, so the rest
//! of the app never touches a COM object.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

use crate::capture::is_access_lost;
use crate::realtime::Realtime;
use crate::probe::{ProbeResult, probe_warm};
use crate::session::{Output, Session, StreamSpec, VideoConfig, VideoFrame, Warm};

/// Longest wait for a desktop change before commands are checked again.
const IDLE_WAIT: Duration = Duration::from_millis(50);
const RESTART_BACKOFF: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
/// A session that ran this long resets the failure count.
const STABLE_RUN: Duration = Duration::from_secs(10);
const IDLE_FOREVER: Duration = Duration::from_secs(3600);
/// How long the desktop may stay unreachable (UAC prompt, lock screen) before the stream restarts.
const RECONNECT_PATIENCE: Duration = Duration::from_secs(10);
const RECONNECT_RETRY: Duration = Duration::from_millis(250);

pub enum Event {
    Started { encoder: String, hardware: bool, width: u32, height: u32, fps: u32 },
    Config(VideoConfig),
    Frame(VideoFrame),
    /// The stream failed; it is retried automatically.
    Error(String),
    Stopped,
}

enum Command {
    Start(StreamSpec),
    Stop,
    ForceKey,
    SetBitrate(u32),
    Probe(StreamSpec, Duration, Sender<Result<Vec<ProbeResult>, String>>),
    Prewarm(StreamSpec),
    Shutdown,
}

pub struct Pipeline {
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Pipeline {
    pub fn spawn(sink: impl Fn(Event) + Send + 'static) -> Pipeline {
        let (commands, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("xiro-video".into())
            .spawn(move || run(rx, sink))
            .expect("spawning the video thread");
        Pipeline { commands, thread: Some(thread) }
    }

    /// Starts streaming, or switches to a new stream (restarting the encoder).
    pub fn start(&self, spec: StreamSpec) {
        let _ = self.commands.send(Command::Start(spec));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    /// The next frame is a key frame (a viewer joined or its decoder reset).
    pub fn force_key(&self) {
        let _ = self.commands.send(Command::ForceKey);
    }

    pub fn set_bitrate(&self, bitrate: u32) {
        let _ = self.commands.send(Command::SetBitrate(bitrate));
    }

    /// Opens an encoder for the expected stream in the background (see [`session::prewarm`]).
    /// `spec.display` only selects the GPU, so any monitor on it will do.
    pub fn prewarm(&self, spec: StreamSpec) {
        let _ = self.commands.send(Command::Prewarm(spec));
    }

    /// A handle that runs encoder tests on the video thread, so the winning encoder is
    /// already open when the stream starts (opening one takes about a second).
    pub fn prober(&self) -> Prober {
        Prober(self.commands.clone())
    }
}

/// Runs encoder tests on the video thread. Cloneable and usable from any other thread.
#[derive(Clone)]
pub struct Prober(Sender<Command>);

impl Prober {
    /// Blocks until the test ran. Stops any running stream first.
    pub fn probe(&self, spec: StreamSpec, duration: Duration) -> Result<Vec<ProbeResult>, String> {
        let (reply, result) = mpsc::channel();
        self.0.send(Command::Probe(spec, duration, reply)).map_err(|_| "the video thread is gone".to_owned())?;
        result.recv().map_err(|_| "the video thread is gone".to_owned())?
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Runner<F: Fn(Event)> {
    sink: F,
    wanted: Option<StreamSpec>,
    session: Option<(Session, Instant)>,
    realtime: Option<Realtime>,
    failures: usize,
    retry_at: Option<Instant>,
    force_key: bool,
    /// Device and encoder from the last stream or test, reused when they fit.
    warm: Option<Warm>,
    /// When the display became unreachable (secure desktop), while waiting for it.
    reconnecting_since: Option<Instant>,
}

fn run(rx: Receiver<Command>, sink: impl Fn(Event)) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Err(err) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            sink(Event::Error(format!("Media Foundation is unavailable: {err}")));
            return;
        }
    }
    let mut runner = Runner { sink, wanted: None, session: None, realtime: None, failures: 0, retry_at: None, force_key: false, warm: None, reconnecting_since: None };
    loop {
        let command = if runner.session.is_some() {
            rx.try_recv().ok()
        } else {
            // Idle (or waiting to retry): block until a command or the retry time.
            let timeout = runner.retry_at.map_or(IDLE_FOREVER, |t| t.saturating_duration_since(Instant::now()));
            match rx.recv_timeout(timeout) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(Command::Shutdown),
            }
        };
        match command {
            Some(Command::Shutdown) => break,
            Some(command) => runner.handle(command),
            None => {}
        }
        runner.tick();
    }
    runner.close();
    unsafe {
        let _ = MFShutdown();
    }
}

impl<F: Fn(Event)> Runner<F> {
    fn handle(&mut self, command: Command) {
        match command {
            // Already streaming (or retrying) exactly this: keep the warm encoder and the backoff.
            Command::Start(spec) if self.wanted.as_ref() == Some(&spec) && (self.session.is_some() || self.retry_at.is_some()) => {
                self.force_key = true;
            }
            Command::Start(spec) => {
                self.close();
                self.failures = 0;
                self.wanted = Some(spec);
                self.retry_at = Some(Instant::now());
            }
            Command::Stop => {
                self.wanted = None;
                self.retry_at = None;
                self.close();
                (self.sink)(Event::Stopped);
            }
            Command::ForceKey => self.force_key = true,
            Command::SetBitrate(bitrate) => {
                if let Some(spec) = &mut self.wanted {
                    spec.bitrate = bitrate;
                }
                let applied = self.session.as_mut().is_some_and(|(s, _)| s.set_bitrate(bitrate));
                if !applied && self.session.is_some() {
                    // The encoder can't change bitrate on the fly: reopen it.
                    self.close();
                    self.retry_at = Some(Instant::now());
                }
            }
            Command::Probe(spec, duration, reply) => {
                // The display is being reshaped: no stream runs while encoders are tested.
                self.wanted = None;
                self.retry_at = None;
                self.close();
                let _realtime = Realtime::enter();
                let _ = reply.send(probe_warm(&spec, duration, &mut self.warm).map_err(|e| e.message()));
            }
            Command::Prewarm(spec) => {
                if self.session.is_none() && self.warm.is_none() {
                    let _realtime = Realtime::enter();
                    self.warm = crate::session::prewarm(&spec).ok();
                }
            }
            Command::Shutdown => {}
        }
    }

    fn tick(&mut self) {
        let force_key = std::mem::take(&mut self.force_key);
        let Some((session, _)) = self.session.as_mut() else {
            if self.retry_at.is_some_and(|t| Instant::now() >= t) {
                self.open();
            }
            return;
        };
        match session.step(force_key, IDLE_WAIT) {
            Ok(outputs) => {
                for output in outputs {
                    (self.sink)(match output {
                        Output::Config(c) => Event::Config(c),
                        Output::Frame(f) => Event::Frame(f),
                    });
                }
            }
            Err(err) if is_access_lost(&err) => match session.reconnect() {
                Ok(()) => self.reconnecting_since = None,
                // A UAC prompt or the lock screen hides the desktop for a while: keep the encoder
                // and try again until it is back, rather than tearing the stream down.
                Err(err) => {
                    let since = *self.reconnecting_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > RECONNECT_PATIENCE {
                        self.reconnecting_since = None;
                        // The encoder is fine; only the display went away. Keep it warm for the retry.
                        if let Some((session, _)) = self.session.take() {
                            self.warm = Some(session.into_warm());
                        }
                        self.fail(format!("Lost the display: {}", err.message()));
                    } else {
                        std::thread::sleep(RECONNECT_RETRY);
                    }
                }
            },
            Err(err) => self.fail(format!("Video encoding failed: {}", err.message())),
        }
    }

    fn open(&mut self) {
        let Some(spec) = self.wanted.clone() else { return };
        self.retry_at = None;
        match Session::open(spec, &mut self.warm) {
            Ok(session) => {
                // A reused encoder is mid-stream: start the new viewers on a key frame.
                self.force_key = true;
                let (width, height) = session.size();
                (self.sink)(Event::Started {
                    encoder: session.encoder_name().to_owned(),
                    hardware: session.is_hardware(),
                    width,
                    height,
                    fps: session.spec.fps,
                });
                self.realtime.get_or_insert_with(Realtime::enter);
                self.session = Some((session, Instant::now()));
            }
            Err(err) => self.fail(format!("Could not start the video stream: {}", err.message())),
        }
    }

    fn fail(&mut self, message: String) {
        if self.session.as_ref().is_some_and(|(_, started)| started.elapsed() > STABLE_RUN) {
            self.failures = 0;
        }
        self.session = None;
        (self.sink)(Event::Error(message));
        if self.wanted.is_some() {
            let delay = RESTART_BACKOFF[self.failures.min(RESTART_BACKOFF.len() - 1)];
            self.failures += 1;
            self.retry_at = Some(Instant::now() + delay);
        }
    }

    /// Ends the stream, keeping its device and encoder warm for the next one.
    fn close(&mut self) {
        if let Some((session, _)) = self.session.take() {
            self.warm = Some(session.into_warm());
        }
        self.realtime = None;
    }
}
