//! Input thread tests, against the fake sink only.

use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP;

use super::*;
use crate::protocol::{Phase, PointerKind};
use crate::records::tests::ki;
use crate::sink::DeviceKind;
use crate::sink::fake::FakeSink;

const TARGET: Rect = Rect { left: 0, top: 0, width: 100, height: 100 };

fn spawn_fake() -> (InputHandle, JoinHandle<()>, FakeSink, mpsc::Receiver<String>) {
    let sink = FakeSink::default();
    let thread_sink = sink.clone();
    let (warn_tx, warn_rx) = mpsc::channel();
    let (handle, join) = InputHandle::spawn_with(
        move || thread_sink,
        false,
        move |m| {
            let _ = warn_tx.send(m);
        },
    );
    (handle, join.expect("thread spawned"), sink, warn_rx)
}

fn touch(phase: Phase) -> Pointer {
    Pointer { kind: PointerKind::Touch, phase, id: 0, x: 0.5, y: 0.5, pressure: 0.0, tilt_x: 0, tilt_y: 0, buttons: 0 }
}

#[test]
fn handle_is_send_sync_clone() {
    fn assert_traits<T: Send + Sync + Clone + 'static>() {}
    assert_traits::<InputHandle>();
}

#[test]
fn commands_run_in_order_and_shutdown_releases_held_keys() {
    let (handle, join, sink, _warn) = spawn_fake();
    handle.key("KeyA".into(), true);
    handle.text("hi".into());
    handle.shutdown();
    join.join().expect("thread exits cleanly");
    let sent = sink.sent();
    assert_eq!(sent.len(), 1 + 4 + 1);
    assert_eq!(ki(&sent[0]).wScan, 0x1e);
    let last = ki(&sent[5]);
    assert_eq!(last.wScan, 0x1e);
    assert!(last.dwFlags.contains(KEYEVENTF_KEYUP));
    handle.key("KeyB".into(), true); // after shutdown: silently dropped
}

#[test]
fn dropping_every_handle_ends_the_thread() {
    let (handle, join, sink, _warn) = spawn_fake();
    let clone = handle.clone();
    handle.set_target(TARGET);
    clone.pointer(touch(Phase::Down));
    drop(handle);
    drop(clone);
    join.join().expect("thread exits cleanly");
    let frames = sink.frames(DeviceKind::Touch);
    let last = frames.last().expect("release frame");
    // SAFETY: touch frames hold `touchInfo`.
    let flags = unsafe { last[0].Anonymous.touchInfo.pointerInfo.pointerFlags.0 };
    assert_ne!(flags & crate::pointer_records::flag::CANCELED, 0, "held finger cancelled on exit");
}

/// Generous upper bound for the input thread to catch up when the machine is loaded.
const PATIENCE: Duration = Duration::from_secs(5);

/// Polls `cond` until it holds or [`PATIENCE`] runs out; timing tests must not flake under load.
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + PATIENCE;
    while Instant::now() < until {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// Blocks until the thread has processed everything sent so far (an F1 tap is the marker).
fn sync(handle: &InputHandle, sink: &FakeSink) {
    let before = sink.sent().len();
    handle.key("F1".into(), true);
    handle.key("F1".into(), false);
    assert!(eventually(|| sink.sent().len() >= before + 2), "input thread stalled");
}

#[test]
fn held_touches_get_a_keepalive_frame_every_50ms() {
    let (handle, join, sink, _warn) = spawn_fake();
    handle.set_target(TARGET);
    let started = Instant::now();
    handle.pointer(touch(Phase::Down));
    assert!(eventually(|| sink.frames(DeviceKind::Touch).len() >= 3), "down plus at least two keepalives");
    assert!(started.elapsed() >= TOUCH_KEEPALIVE * 2, "keepalives are paced, not spammed");

    handle.pointer(touch(Phase::Up));
    handle.refresh_layout();
    sync(&handle, &sink);
    let after_up = sink.frames(DeviceKind::Touch).len();
    thread::sleep(TOUCH_KEEPALIVE * 3);
    assert_eq!(sink.frames(DeviceKind::Touch).len(), after_up, "keepalive stops once lifted");
    handle.release_all();
    handle.shutdown();
    join.join().expect("thread exits cleanly");
}

#[test]
fn keepalive_is_not_starved_by_a_message_flood() {
    let (handle, join, sink, _warn) = spawn_fake();
    handle.set_target(TARGET);
    handle.pointer(touch(Phase::Down));
    let fired = eventually(|| {
        // Each zero-delta wheel is one cheap move record, sent faster than the keepalive period.
        handle.wheel(Wheel { x: 0.0, y: 0.0, dx: 0, dy: 0 });
        sink.frames(DeviceKind::Touch).len() >= 2
    });
    assert!(fired, "keepalive fired despite traffic");
    handle.shutdown();
    join.join().expect("thread exits cleanly");
}

#[test]
fn warnings_reach_the_callback_once() {
    let (handle, join, _sink, warn) = spawn_fake();
    handle.pointer(touch(Phase::Down)); // no target yet
    handle.pointer(touch(Phase::Move));
    handle.shutdown();
    join.join().expect("thread exits cleanly");
    let got: Vec<String> = warn.try_iter().collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("no target monitor"));
}
