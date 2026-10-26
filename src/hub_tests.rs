// Hub scenario tests.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use super::*;
use crate::streamer::tests::FakeLink;

const IPAD: Screen = Screen { width: 2360, height: 1640, hz: 120, scale: 200 };
const PORTRAIT: Screen = Screen { width: 1640, height: 2360, hz: 120, scale: 200 };
const SMALL: Screen = Screen { width: 2048, height: 1536, hz: 60, scale: 200 };

#[derive(Default)]
struct FakeProvider {
    mirrored: bool,
    fail: bool,
    /// Holds acquire() until released, to test shutdown during a setup.
    hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    acquired: Mutex<Vec<Option<Screen>>>,
    released: AtomicUsize,
}

impl Provider for FakeProvider {
    fn is_virtual(&self) -> bool {
        !self.mirrored
    }
    fn acquire(&self, screen: Option<Screen>, progress: &dyn Fn(&str), _cancel: &AtomicBool) -> Result<Stream, String> {
        self.acquired.lock().unwrap().push(screen);
        progress("Setting up…");
        if let Some(rx) = self.hold.lock().unwrap().take() {
            let _ = rx.recv();
        }
        if self.fail {
            return Err("driver missing".into());
        }
        let s = screen.unwrap_or(IPAD);
        Ok(Stream { name: "V".into(), width: s.width, height: s.height, fps: 60, bitrate: 25_000_000, encoder: EncoderPreference::Auto })
    }
    fn release(&self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct FakeVideo {
    starts: Mutex<Vec<StreamSpec>>,
    stops: AtomicUsize,
    bitrates: Mutex<Vec<u32>>,
}

impl Video for FakeVideo {
    fn start(&self, spec: StreamSpec) {
        self.starts.lock().unwrap().push(spec);
    }
    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn set_bitrate(&self, bitrate: u32) {
        self.bitrates.lock().unwrap().push(bitrate);
    }
}

#[derive(Default)]
struct FakeInput {
    releases: AtomicUsize,
    targets: Mutex<Vec<String>>,
}

impl Input for FakeInput {
    fn release_all(&self) {
        self.releases.fetch_add(1, Ordering::SeqCst);
    }
    fn target(&self, gdi_name: &str) {
        self.targets.lock().unwrap().push(gdi_name.to_owned());
    }
}

struct Rig {
    tx: mpsc::UnboundedSender<Msg>,
    controller: Arc<AtomicU64>,
    provider: Arc<FakeProvider>,
    video: Arc<FakeVideo>,
    input: Arc<FakeInput>,
    streamer: Arc<Mutex<Streamer>>,
    links: Mutex<Vec<Arc<FakeLink>>>,
}

impl Rig {
    fn new(provider: FakeProvider) -> Rig {
        let provider = Arc::new(provider);
        let (video, input) = (Arc::new(FakeVideo::default()), Arc::new(FakeInput::default()));
        let streamer = Arc::new(Mutex::new(Streamer::new(20_000_000)));
        let config = HubConfig { idle_stop: Duration::from_millis(30), gop_seconds: 1.0, on_status: Arc::new(|_| {}) };
        let (hub, rx, tx, controller) = Hub::new(provider.clone(), video.clone(), input.clone(), streamer.clone(), config);
        tokio::spawn(hub.run(rx));
        Rig { tx, controller, provider, video, input, streamer, links: Mutex::new(Vec::new()) }
    }

    fn join(&self, id: ViewerId) -> Arc<FakeLink> {
        let link = Arc::new(FakeLink::default());
        self.streamer.lock().unwrap().add(id, link.clone());
        self.links.lock().unwrap().push(link.clone());
        self.tx.send(Msg::Join(id)).unwrap();
        link
    }

    fn leave(&self, id: ViewerId) {
        self.streamer.lock().unwrap().remove(id);
        self.tx.send(Msg::Leave(id)).unwrap();
    }

    fn send(&self, msg: Msg) {
        self.tx.send(msg).unwrap();
    }

    fn acquired(&self) -> Vec<Option<Screen>> {
        self.provider.acquired.lock().unwrap().clone()
    }

    fn starts(&self) -> Vec<StreamSpec> {
        self.video.starts.lock().unwrap().clone()
    }

    async fn shutdown(&self) {
        let (done, wait) = oneshot::channel();
        self.send(Msg::Shutdown(done));
        let _ = wait.await;
    }
}

/// Waits (real time) until the condition holds; the hub works on other tasks and threads.
async fn eventually(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Lets queued hub messages run.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(40)).await;
}

fn texts(link: &FakeLink) -> Vec<Value> {
    link.text.lock().unwrap().iter().map(|t| serde_json::from_str(t).unwrap()).collect()
}

#[tokio::test]
async fn the_ipad_screen_shapes_the_display_then_the_stream_starts() {
    let rig = Rig::new(FakeProvider::default());
    let link = rig.join(1);
    settle().await;
    assert!(rig.starts().is_empty(), "nothing runs until the iPad describes its screen");
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream start", || !rig.starts().is_empty()).await;
    assert_eq!(rig.acquired(), vec![Some(IPAD)]);
    assert_eq!(*rig.input.targets.lock().unwrap(), vec!["V".to_owned()], "input maps to the new monitor");
    let spec = &rig.starts()[0];
    assert_eq!((spec.display.as_str(), spec.width, spec.fps, spec.gop, spec.bitrate), ("V", 2360, 60, 60, 25_000_000));
    eventually("welcome", || texts(&link).iter().any(|m| m["t"] == "welcome" && m["display"]["name"] == "V")).await;
    let msgs = texts(&link);
    assert!(msgs.iter().any(|m| m["t"] == "status" && m["text"] == "Setting up…"), "progress reaches the iPad");
    assert!(msgs[0]["hostTime"].is_number(), "hello is answered right away");
    let welcome = msgs.iter().rev().find(|m| m["t"] == "welcome").unwrap();
    assert_eq!(welcome["display"], json!({ "name": "V", "width": 2360, "height": 1640 }));
    assert_eq!(welcome["virtual"], true);
}

#[tokio::test]
async fn reconnecting_with_the_same_screen_reuses_the_display_rotating_reshapes_it() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("first stream", || rig.starts().len() == 1).await;
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("restart", || rig.starts().len() == 2).await;
    assert_eq!(rig.acquired().len(), 1, "same screen: display reused");
    rig.send(Msg::Screen(1, Some(PORTRAIT)));
    eventually("rotation", || rig.acquired().len() == 2).await;
    assert_eq!(rig.acquired()[1], Some(PORTRAIT));
    rig.send(Msg::Screen(1, None));
    settle().await;
    assert_eq!(rig.acquired().len(), 2, "an invalid screen is ignored");
}

#[tokio::test]
async fn only_the_controlling_viewer_shapes_the_display() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.join(2);
    rig.send(Msg::Hello(1, Some(SMALL)));
    rig.send(Msg::Screen(1, Some(IPAD)));
    settle().await;
    assert!(rig.acquired().is_empty());
}

#[tokio::test]
async fn a_failure_is_reported_and_the_monitor_is_turned_off() {
    let rig = Rig::new(FakeProvider { fail: true, ..Default::default() });
    let link = rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("release after failure", || rig.provider.released.load(Ordering::SeqCst) == 1).await;
    let status = texts(&link).into_iter().rev().find(|m| m["t"] == "status").unwrap();
    assert_eq!(status["error"], true);
    assert!(status["detail"].as_str().unwrap().contains("driver missing"));
    assert!(rig.starts().is_empty());
}

#[tokio::test]
async fn the_newest_viewer_controls_input_and_control_passes_back() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.join(2);
    eventually("controller", || rig.controller.load(Ordering::SeqCst) == 2).await;
    assert_eq!(rig.input.releases.load(Ordering::SeqCst), 1, "previous controller input released on hand-over");
    rig.leave(2);
    eventually("back to 1", || rig.controller.load(Ordering::SeqCst) == 1).await;
    assert_eq!(rig.input.releases.load(Ordering::SeqCst), 2);
    rig.leave(1);
    eventually("nobody", || rig.controller.load(Ordering::SeqCst) == NO_CONTROLLER).await;
}

#[tokio::test]
async fn a_non_controlling_viewer_leaving_keeps_the_controllers_input() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.join(2);
    settle().await;
    let before = rig.input.releases.load(Ordering::SeqCst);
    rig.leave(1);
    settle().await;
    assert_eq!(rig.input.releases.load(Ordering::SeqCst), before);
    assert_eq!(rig.controller.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn after_the_last_viewer_leaves_the_virtual_monitor_goes_away() {
    let rig = Rig::new(FakeProvider::default());
    let link = rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream", || rig.starts().len() == 1).await;
    rig.streamer.lock().unwrap().set_config(xiro_video::VideoConfig { codec: "avc1.640028".into(), avcc: vec![1, 0x64, 0, 0x28] });
    rig.leave(1);
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(rig.provider.released.load(Ordering::SeqCst), 0, "a grace period survives Wi-Fi blips");
    eventually("release", || rig.provider.released.load(Ordering::SeqCst) == 1).await;
    assert!(rig.video.stops.load(Ordering::SeqCst) >= 1);
    assert!(!rig.streamer.lock().unwrap().has_config(), "no stale frames for the next viewer");
    drop(link);
}

#[tokio::test]
async fn rejoining_within_the_grace_period_keeps_the_display() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream", || rig.starts().len() == 1).await;
    rig.leave(1);
    rig.join(2);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(rig.provider.released.load(Ordering::SeqCst), 0);
    rig.send(Msg::Hello(2, Some(IPAD)));
    eventually("resume", || rig.starts().len() == 2).await;
    assert_eq!(rig.acquired().len(), 1);
}

#[tokio::test]
async fn shutdown_gives_the_screen_layout_back_and_blocks_new_work() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream", || rig.starts().len() == 1).await;
    rig.shutdown().await;
    assert_eq!(rig.provider.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_during_a_setup_abandons_it_and_leaves_the_monitor_off() {
    let (finish, hold) = std::sync::mpsc::channel();
    let rig = Rig::new(FakeProvider { hold: Mutex::new(Some(hold)), ..Default::default() });
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("setup running", || rig.acquired().len() == 1).await;
    let (done, wait) = oneshot::channel();
    rig.send(Msg::Shutdown(done));
    let _ = wait.await;
    finish.send(()).unwrap();
    eventually("released after the late setup", || rig.provider.released.load(Ordering::SeqCst) == 2).await;
    assert!(rig.starts().is_empty(), "nothing starts after shutdown");
}

#[tokio::test]
async fn a_burst_of_screen_changes_collapses_into_the_newest() {
    let (finish, hold) = std::sync::mpsc::channel();
    let rig = Rig::new(FakeProvider { hold: Mutex::new(Some(hold)), ..Default::default() });
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("first setup running", || rig.acquired().len() == 1).await;
    rig.send(Msg::Screen(1, Some(PORTRAIT)));
    rig.send(Msg::Screen(1, Some(Screen { hz: 60, ..IPAD })));
    let last = Screen { width: 1640, height: 2360, hz: 60, scale: 200 };
    rig.send(Msg::Screen(1, Some(last)));
    settle().await;
    finish.send(()).unwrap();
    eventually("second setup", || rig.acquired().len() == 2).await;
    settle().await;
    assert_eq!(rig.acquired(), vec![Some(IPAD), Some(last)], "the in-between rotations never run");
}

#[tokio::test]
async fn when_the_controlling_ipad_leaves_the_display_reshapes_for_the_next() {
    let rig = Rig::new(FakeProvider::default());
    rig.join(1);
    rig.send(Msg::Hello(1, Some(SMALL)));
    eventually("first", || rig.acquired().len() == 1).await;
    rig.join(2);
    rig.send(Msg::Hello(2, Some(IPAD)));
    eventually("second", || rig.acquired().len() == 2).await;
    rig.leave(2);
    eventually("back to the first iPad", || rig.acquired().len() == 3).await;
    assert_eq!(rig.acquired()[2], Some(SMALL));
}

#[tokio::test]
async fn a_mirrored_display_ignores_rotation() {
    let rig = Rig::new(FakeProvider { mirrored: true, ..Default::default() });
    rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream", || rig.starts().len() == 1).await;
    rig.send(Msg::Screen(1, Some(PORTRAIT)));
    settle().await;
    assert_eq!(rig.acquired().len(), 1);
}

#[tokio::test]
async fn settings_change_the_stream_only_when_something_changes() {
    let rig = Rig::new(FakeProvider::default());
    let link = rig.join(1);
    rig.send(Msg::Hello(1, Some(IPAD)));
    eventually("stream", || rig.starts().len() == 1).await;
    rig.send(Msg::Settings(1, Settings { fps: Some(60), bitrate_mbps: Some(25.0) }));
    settle().await;
    assert_eq!(rig.starts().len(), 1, "no-op settings change nothing");
    rig.send(Msg::Settings(1, Settings { fps: None, bitrate_mbps: Some(40.0) }));
    eventually("bitrate", || rig.video.bitrates.lock().unwrap().last() == Some(&40_000_000)).await;
    rig.send(Msg::Settings(1, Settings { fps: Some(120), bitrate_mbps: None }));
    eventually("restart at 120", || rig.starts().last().is_some_and(|s| s.fps == 120 && s.gop == 120)).await;
    eventually("welcome", || texts(&link).iter().any(|m| m["t"] == "welcome" && m["bitrateMbps"] == 40.0)).await;
    rig.join(2);
    rig.send(Msg::Settings(1, Settings { fps: Some(30), bitrate_mbps: None }));
    settle().await;
    assert_ne!(rig.starts().last().unwrap().fps, 30, "only the controller changes settings");
}

#[tokio::test]
async fn the_status_line_follows_viewers_and_the_stream() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let streamer = Arc::new(Mutex::new(Streamer::new(1)));
    let config = HubConfig {
        idle_stop: Duration::from_millis(30),
        gop_seconds: 1.0,
        on_status: Arc::new(move |s| {
            let _ = tx.send(s);
        }),
    };
    let (mut hub, _, _, _) = Hub::new(Arc::new(FakeProvider::default()), Arc::new(FakeVideo::default()), Arc::new(FakeInput::default()), streamer, config);
    assert_eq!(hub.status_line(), "Waiting for an iPad or iPhone");
    hub.on_join(1);
    assert_eq!(rx.recv().await.unwrap(), "1 device connected · starting…");
    hub.on_join(2);
    hub.stream = Some((Stream { name: "V".into(), width: 2360, height: 1640, fps: 60, bitrate: 1, encoder: EncoderPreference::Auto }, None));
    assert_eq!(hub.status_line(), "2 devices connected · 2360×1640 · 60 fps");
}

#[test]
fn encoder_labels_are_short() {
    assert_eq!(encoder_label("Intel® Quick Sync Video H.264 Encoder MFT", true), "Intel® Quick Sync Video (hardware)");
    assert_eq!(encoder_label("H264 Encoder MFT", false), "Windows software encoder");
    assert_eq!(encoder_label(" MFT", true), "Hardware encoder");
}
