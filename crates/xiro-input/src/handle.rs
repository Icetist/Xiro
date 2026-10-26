//! The dedicated input thread. All injection happens there, so a slow SendInput never stalls the
//! network path, and the injector state needs no locking.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::geometry::Rect;
use crate::injector::Injector;
use crate::protocol::{Pointer, Wheel};
use crate::sink::InputSink;
use crate::win32::Win32Sink;

/// Windows cancels touch contacts that go quiet; re-send held contacts this often.
pub(crate) const TOUCH_KEEPALIVE: Duration = Duration::from_millis(50);
const THREAD_NAME: &str = "xiro-input";

enum Command {
    Target(Rect),
    RefreshLayout,
    Pointer(Pointer),
    Wheel(Wheel),
    Key(String, bool),
    Text(String),
    ReleaseAll,
    Shutdown,
}

/// Non-blocking sends to the input thread; dropped silently once it has ended.
/// Dropping the last handle releases everything and ends the thread, like [`Self::shutdown`].
#[derive(Clone)]
pub struct InputHandle {
    tx: Sender<Command>,
}

type Warn = Arc<Mutex<dyn Fn(String) + Send>>;

fn emit(warn: &Warn, message: String) {
    // A poisoned lock only means an earlier warning callback panicked; keep reporting.
    let f = warn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(message);
}

impl InputHandle {
    /// Starts the input thread against the real desktop. `cmd_as_ctrl` maps the iPad Cmd keys to Ctrl.
    pub fn spawn(cmd_as_ctrl: bool, on_warning: impl Fn(String) + Send + 'static) -> InputHandle {
        Self::spawn_with(Win32Sink::default, cmd_as_ctrl, on_warning).0
    }

    /// `make_sink` runs on the input thread, so the sink itself need not be `Send`.
    pub(crate) fn spawn_with<S, F>(make_sink: F, cmd_as_ctrl: bool, on_warning: impl Fn(String) + Send + 'static) -> (InputHandle, Option<JoinHandle<()>>)
    where
        S: InputSink,
        F: FnOnce() -> S + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let warn: Warn = Arc::new(Mutex::new(on_warning));
        let thread_warn = Arc::clone(&warn);
        let spawned = thread::Builder::new().name(THREAD_NAME.to_owned()).spawn(move || {
            let w = Arc::clone(&thread_warn);
            let injector = Injector::new(make_sink(), cmd_as_ctrl, Box::new(move |m| emit(&w, m)));
            run(injector, rx);
        });
        match spawned {
            Ok(join) => (InputHandle { tx }, Some(join)),
            Err(e) => {
                emit(&warn, format!("Input thread could not start ({e}); remote input is disabled."));
                (InputHandle { tx }, None)
            }
        }
    }

    fn send(&self, cmd: Command) {
        // Err only means the thread has ended; see the type docs.
        let _ = self.tx.send(cmd);
    }

    /// The streamed monitor, in physical pixels.
    pub fn set_target(&self, rect: Rect) {
        self.send(Command::Target(rect));
    }

    /// Virtual-screen metrics changed (monitor added, resolution switch...).
    pub fn refresh_layout(&self) {
        self.send(Command::RefreshLayout);
    }

    pub fn pointer(&self, p: Pointer) {
        self.send(Command::Pointer(p));
    }

    pub fn wheel(&self, w: Wheel) {
        self.send(Command::Wheel(w));
    }

    /// `code` is a `KeyboardEvent.code`; unmapped codes are ignored.
    pub fn key(&self, code: String, down: bool) {
        self.send(Command::Key(code, down));
    }

    /// Types `s` as Unicode (first 256 UTF-16 units).
    pub fn text(&self, s: String) {
        self.send(Command::Text(s));
    }

    /// Lift every held key, button, finger and pen (e.g. the client disconnected).
    pub fn release_all(&self) {
        self.send(Command::ReleaseAll);
    }

    /// Releases everything and ends the thread. Other clones become inert.
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }
}

fn apply<S: InputSink>(injector: &mut Injector<S>, cmd: Command) {
    match cmd {
        Command::Target(rect) => injector.set_target(rect),
        Command::RefreshLayout => injector.refresh_layout(),
        Command::Pointer(p) => injector.pointer(&p),
        Command::Wheel(w) => injector.wheel(&w),
        Command::Key(code, down) => injector.key(&code, down),
        Command::Text(s) => injector.text(&s),
        Command::ReleaseAll => injector.release_all(),
        // `run` intercepts Shutdown before it gets here.
        Command::Shutdown => {}
    }
}

/// While touches are held, wakes at least every [`TOUCH_KEEPALIVE`], timed from the last
/// keepalive so a flood of other messages can't starve it.
fn run<S: InputSink>(mut injector: Injector<S>, rx: Receiver<Command>) {
    let mut next_keepalive: Option<Instant> = None;
    loop {
        let received = if injector.is_touching() {
            let deadline = *next_keepalive.get_or_insert_with(|| Instant::now() + TOUCH_KEEPALIVE);
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            next_keepalive = None;
            match rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => break,
            }
        };
        match received {
            Some(Command::Shutdown) => break,
            Some(cmd) => apply(&mut injector, cmd),
            None => {}
        }
        let due = next_keepalive.is_some_and(|d| Instant::now() >= d);
        if due && injector.is_touching() {
            injector.keepalive();
            next_keepalive = Some(Instant::now() + TOUCH_KEEPALIVE);
        }
    }
    injector.release_all();
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod tests;
