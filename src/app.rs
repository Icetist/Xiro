//! Startup wiring (settings, certificates, stream source, video, input, hub, servers)
//! and a shutdown that always restores the PC's screen layout.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use xiro_display::VirtualDisplay;
use xiro_input::{InputHandle, MonitorChoice};
use xiro_video::{EncoderPreference, Event, Pipeline, StreamSpec};

use crate::cli::Cli;
use crate::config::{Config, DisplayChoice, Fps};
use crate::hub::{self, Hub, HubConfig, Msg};
use crate::identity::Identity;
use crate::log;
use crate::pairing;
use crate::provider::{MirrorProvider, ModeCache, SidecarProvider, StreamSettings, real_probe};
use crate::server;
use crate::streamer::Streamer;
use crate::viewer;

/// How long the virtual monitor stays after the iPad leaves (Wi-Fi blips, app switches).
const IDLE_STOP: Duration = Duration::from_secs(15);
const MIRROR_FPS: u32 = 60;
/// Right after sign-in Windows may not have started the display driver yet.
const DRIVER_WAIT: Duration = Duration::from_secs(30);
const DRIVER_POLL: Duration = Duration::from_secs(2);
const INITIAL_BITRATE: u32 = 20_000_000;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// Devices watching at once (each costs bandwidth; one controls the PC).
const MAX_VIEWERS: usize = 4;
/// How often the network is re-read (one cheap system call when nothing changed).
const NETWORK_POLL: Duration = Duration::from_secs(3);

struct VideoAdapter(Pipeline);

impl hub::Video for VideoAdapter {
    fn start(&self, spec: StreamSpec) {
        self.0.start(spec);
    }
    fn stop(&self) {
        self.0.stop();
    }
    fn set_bitrate(&self, bitrate: u32) {
        self.0.set_bitrate(bitrate);
    }
}

struct InputAdapter(InputHandle);

impl hub::Input for InputAdapter {
    fn release_all(&self) {
        self.0.release_all();
    }
    fn target(&self, gdi_name: &str) {
        self.0.refresh_layout();
        match xiro_input::list_monitors().into_iter().find(|m| m.name.eq_ignore_ascii_case(gdi_name)) {
            Some(monitor) => self.0.set_target(monitor.rect),
            None => log::warning!("Input: {gdi_name} is not among the monitors; touches may land on the wrong screen"),
        }
    }
}

/// Re-reads the network every few seconds so the PC stays reachable when networks change.
async fn follow_network(identity: Arc<Identity>) {
    let mut ticks = tokio::time::interval(NETWORK_POLL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let check = identity.clone();
        match tokio::task::spawn_blocking(move || check.refresh()).await {
            Ok(Ok(Some(now))) => log::info!("Network changed: devices now reach this PC at {}", now.host),
            Ok(Err(err)) => log::warning!("Network changed, but the certificate could not be updated: {err}"),
            _ => {}
        }
    }
}

pub fn banner(identity: &Identity, details: &[String]) -> Result<(), String> {
    let urls = identity.urls(false)?;
    let now = identity.current();
    log::raw("");
    log::raw("  Xiro: your iPad as a second monitor. No app, no account.");
    log::raw("");
    for line in details {
        log::raw(&format!("  {line}"));
    }
    log::raw("");
    log::raw("  Scan with the iPad camera (iPad must be on the same Wi-Fi / hotspot / USB network):");
    for line in pairing::qr_lines(&urls.setup_url) {
        log::raw(&format!("    {line}"));
    }
    log::raw(&format!("  Or open in Safari: {}", urls.setup_url));
    log::raw(&format!("  Already set up?    {}", urls.app_url));
    if now.addresses.is_empty() {
        log::raw("  No network yet: connect this PC to Wi-Fi or a hotspot (Xiro picks it up automatically).");
    } else if now.addresses.len() > 1 {
        let others: Vec<String> = now.addresses[1..].iter().map(|a| format!("{} ({})", a.address, a.name)).collect();
        log::raw(&format!("  Other addresses: {}", others.join(", ")));
    }
    log::raw("");
    log::raw("  When installing the certificate, the iPad shows its SHA-256 fingerprint under \"More Details\".");
    log::raw("  It must match this one exactly:");
    log::raw(&format!("    {}", now.certs.ca_fingerprint));
    log::raw("");
    Ok(())
}

pub fn encoder_preference(name: &str) -> EncoderPreference {
    match name.to_ascii_lowercase().as_str() {
        "auto" => EncoderPreference::Auto,
        "hardware" | "hw" => EncoderPreference::Hardware,
        "software" | "sw" => EncoderPreference::Software,
        // Legacy encoder ids from old settings files.
        "nvenc" | "nvenc-copy" | "amf" | "amf-copy" | "qsv" | "mf" => EncoderPreference::Hardware,
        "x264" | "x264-gpucsc" => EncoderPreference::Software,
        other => EncoderPreference::Named(other.to_owned()),
    }
}

fn driver_present(display: &VirtualDisplay, wait: bool) -> bool {
    let deadline = std::time::Instant::now() + if wait { DRIVER_WAIT } else { Duration::ZERO };
    loop {
        match display.available() {
            Ok(true) => return true,
            Ok(false) => {}
            Err(err) => log::debug!("Display query failed: {err}"),
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(DRIVER_POLL);
    }
}

/// Virtual monitor when the driver is installed, else an existing display.
/// Also returns the stream to pre-warm the encoder for, if predictable.
fn provider(
    cli: &Cli,
    config: &Config,
    dir: &std::path::Path,
    prober: xiro_video::Prober,
) -> Result<(Arc<dyn hub::Provider>, String, Option<StreamSpec>), String> {
    let fps = cli.fps.map(Fps::Cap).unwrap_or(config.fps);
    let settings = StreamSettings {
        fps_cap: match fps {
            Fps::Auto => None,
            Fps::Cap(f) => Some(f),
        },
        bitrate_mbps: cli.bitrate_mbps.or(config.bitrate_mbps),
        encoder: encoder_preference(cli.encoder.as_deref().unwrap_or(&config.encoder)),
        gop_seconds: config.gop_seconds,
    };
    let modes = ModeCache::new(dir.join("mode-cache.json"));
    if cli.reprobe {
        modes.clear();
    }
    let choice = cli.display.clone().unwrap_or_else(|| config.display.clone());
    if choice == DisplayChoice::Auto {
        let display = VirtualDisplay::new(dir.join("layout.json"));
        if driver_present(&display, cli.background || cli.wait_for_driver) {
            // The monitor only exists while a device is connected (this also cleans up after a crash).
            if let Err(err) = display.detach() {
                log::warning!("Could not remove a leftover virtual monitor: {err}");
            }
            let sidecar = SidecarProvider::new(display, settings, modes, real_probe(prober));
            // The virtual monitor is off until a device connects; any monitor selects the same GPU.
            let primary = xiro_input::list_monitors().into_iter().find(|m| m.is_primary).map(|m| m.name);
            let warm = primary.and_then(|name| sidecar.likely_stream(&name));
            let detail = "Display : virtual, shaped like your device (appears when it connects)".into();
            return Ok((Arc::new(sidecar), detail, warm));
        }
    }
    let monitors = xiro_input::list_monitors();
    let pick = match &choice {
        DisplayChoice::Auto => MonitorChoice::Auto,
        DisplayChoice::Index(i) => MonitorChoice::Index(*i),
        DisplayChoice::Name(n) => MonitorChoice::Name(n.clone()),
    };
    let monitor = xiro_input::pick_monitor(&monitors, &pick)?;
    if !monitor.is_virtual && monitors.len() == 1 {
        log::warning!("No virtual display driver found, so the device mirrors this screen. Open Xiro and choose Fix for a true second screen.");
    }
    let fps = settings.fps_cap.unwrap_or(MIRROR_FPS);
    let detail = format!("Display : mirroring {} ({}x{})", monitor.name, monitor.rect.width, monitor.rect.height);
    let (width, height) = (monitor.rect.width.max(0) as u32, monitor.rect.height.max(0) as u32);
    let mirror = MirrorProvider::new(monitor.name, width, height, fps, settings, real_probe(prober));
    Ok((Arc::new(mirror), detail, None))
}

/// Everything that runs. Call [`Running::shutdown`] to stop it.
pub struct Running {
    hub: mpsc::UnboundedSender<Msg>,
    input: InputHandle,
    runtime: tokio::runtime::Runtime,
    streamer: Arc<Mutex<Streamer>>,
    token: Arc<crate::config::PairingToken>,
    codes: Arc<crate::pairing::PairingCodes>,
}

impl Running {
    /// Rotates the pairing token and disconnects every device. Returns how many were connected.
    pub fn unlink_all(&self) -> Result<usize, String> {
        self.token.rotate().map_err(|e| format!("Could not save a new pairing key: {e}"))?;
        if let Err(err) = self.codes.clear() {
            log::warning!("Could not clear unused pairing codes: {err}");
        }
        let dropped = self.streamer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unlink_all();
        log::info!("Unlinked every device ({dropped} connected)");
        Ok(dropped)
    }

    /// Stops encoding, abandons display setup, turns the virtual monitor off and lifts held input.
    pub fn shutdown(self) {
        log::info!("Shutting down...");
        let (done, wait) = oneshot::channel();
        if self.hub.send(Msg::Shutdown(done)).is_ok() {
            let _ = self.runtime.block_on(async { tokio::time::timeout(SHUTDOWN_WAIT, wait).await });
        }
        self.input.shutdown();
        self.runtime.shutdown_timeout(Duration::from_millis(300));
    }
}

/// How the running app talks to the person at the PC.
pub struct Callbacks {
    /// The tray's status line.
    pub on_status: Arc<dyn Fn(String) + Send + Sync>,
    /// A device connected (and took control).
    pub on_join: Arc<dyn Fn(&str) + Send + Sync>,
    /// May the device at this address pair? Blocking.
    pub approve: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

/// Frames go straight from the video thread to viewers' queues, bypassing the hub.
fn spawn_pipeline(streamer: Arc<Mutex<Streamer>>, hub: Arc<OnceLock<mpsc::UnboundedSender<Msg>>>) -> Pipeline {
    let tell_hub = move |msg: Msg| {
        if let Some(tx) = hub.get() {
            let _ = tx.send(msg);
        }
    };
    Pipeline::spawn(move |event| match event {
        Event::Frame(frame) => {
            if let Ok(mut s) = streamer.lock() {
                s.push(&frame, frame.captured_at);
            }
        }
        Event::Config(config) => {
            if let Ok(mut s) = streamer.lock() {
                s.set_config(config);
            }
        }
        Event::Started { encoder, hardware, width, height, fps } => {
            log::info!("Encoding {width}x{height} at {fps} fps with {encoder}");
            tell_hub(Msg::VideoStarted { encoder, hardware });
        }
        Event::Error(message) => tell_hub(Msg::VideoError(message)),
        Event::Stopped => {}
    })
}

pub fn start(cli: &Cli, dir: PathBuf, config: &Config, identity: &Arc<Identity>, callbacks: Callbacks) -> Result<(Running, String), String> {
    let Callbacks { on_status, on_join, approve } = callbacks;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("xiro-net")
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start: {e}"))?;

    let streamer = Arc::new(Mutex::new(Streamer::new(INITIAL_BITRATE)));
    let hub_tx: Arc<OnceLock<mpsc::UnboundedSender<Msg>>> = Arc::new(OnceLock::new());
    let pipeline = spawn_pipeline(streamer.clone(), hub_tx.clone());
    let prober = pipeline.prober();
    let video = Arc::new(VideoAdapter(pipeline));
    let (provider, detail, warm) = provider(cli, config, &dir, prober)?;
    if let Some(spec) = warm {
        video.0.prewarm(spec);
    }
    let input = InputHandle::spawn(config.cmd_as_ctrl, |warning| log::warning!("{warning}"));
    let hub_config = HubConfig { idle_stop: IDLE_STOP, gop_seconds: config.gop_seconds, on_status };
    let (hub, rx, tx, controller) = Hub::new(provider, video.clone(), Arc::new(InputAdapter(input.clone())), streamer.clone(), hub_config);
    let _ = hub_tx.set(tx.clone());
    runtime.spawn(hub.run(rx));

    let key_video = video.clone();
    let viewer_ctx = Arc::new(viewer::Context {
        streamer: streamer.clone(),
        hub: tx.clone(),
        controller,
        input: input.clone(),
        force_key: Arc::new(move || key_video.0.force_key()),
        next_id: AtomicU64::new(1),
        max_viewers: MAX_VIEWERS,
        on_join,
    });
    let shared = Arc::new(server::Shared {
        token: identity.token.clone(),
        codes: identity.codes.clone(),
        ca_der: identity.current().certs.ca_der.clone(),
        https_port: config.https_port,
        viewer: viewer_ctx,
        limiter: Mutex::new(server::AuthLimiter::new()),
        approve,
    });
    let tls_source = identity.clone();
    let tls = move || tls_source.current().tls.clone();
    runtime.block_on(server::start(shared, tls, config.http_port))?;
    runtime.spawn(follow_network(identity.clone()));
    let (token, codes) = (identity.token.clone(), identity.codes.clone());
    Ok((Running { hub: tx, input, runtime, streamer, token, codes }, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_names_from_older_configs_still_mean_something() {
        assert_eq!(encoder_preference("auto"), EncoderPreference::Auto);
        assert_eq!(encoder_preference("NVENC"), EncoderPreference::Hardware);
        assert_eq!(encoder_preference("x264"), EncoderPreference::Software);
        assert_eq!(encoder_preference("Quick Sync"), EncoderPreference::Named("quick sync".into()));
    }
}
