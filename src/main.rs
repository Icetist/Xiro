//! Xiro: an iPad or iPhone (Safari, no app) as a low-latency second monitor for Windows.
//! A tray app by default; `--console` runs it in a terminal instead.

#![windows_subsystem = "windows"]

mod app;
mod assets;
mod autostart;
mod cli;
mod config;
mod desk;
mod http;
mod hub;
mod identity;
mod log;
mod network;
mod pairing;
mod policy;
mod protocol;
mod provider;
mod screens;
mod server;
mod setup;
mod streamer;
mod viewer;
mod xiro_wifi;

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler, SetConsoleOutputCP,
};
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{BOOL, w};
use desk::TITLE;
use identity::Identity;
use xiro_shell::{Status, install};
use xiro_wifi::XiroWifi;

const CP_UTF8: u32 = 65001;
/// A restarted Xiro waits this long for the one that started it to exit.
const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const RESTART_POLL: std::time::Duration = std::time::Duration::from_millis(200);

static STOP: std::sync::OnceLock<mpsc::Sender<()>> = std::sync::OnceLock::new();

/// Ctrl+C, Ctrl+Break, closing the console window, sign-out and shutdown all clean up.
unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
    let handled = [CTRL_C_EVENT, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT].contains(&event);
    if handled {
        if let Some(stop) = STOP.get() {
            let _ = stop.send(());
        }
        // Windows ends the process when this handler returns for close/logoff/shutdown: give cleanup time.
        if event == CTRL_CLOSE_EVENT || event == CTRL_LOGOFF_EVENT || event == CTRL_SHUTDOWN_EVENT {
            std::thread::sleep(std::time::Duration::from_secs(4));
        }
    }
    handled.into()
}

/// Output goes to the terminal Xiro was started from, if any (it is a GUI program).
fn attach_console() -> bool {
    unsafe {
        let attached = AttachConsole(ATTACH_PARENT_PROCESS).is_ok();
        if attached {
            // The pairing QR code is drawn with Unicode block characters.
            let _ = SetConsoleOutputCP(CP_UTF8);
        }
        attached
    }
}

/// One Xiro per user; `wait` lets an exiting Xiro (the one that started this) finish first.
fn first_instance(wait: bool) -> bool {
    let deadline = std::time::Instant::now() + if wait { RESTART_WAIT } else { std::time::Duration::ZERO };
    loop {
        // SAFETY: plain calls; a handle to someone else's mutex is closed again at once.
        let first = unsafe {
            let handle = CreateMutexW(None, true, w!("Local\\Xiro.Host"));
            let first = handle.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS;
            if let (false, Ok(other)) = (first, handle) {
                let _ = CloseHandle(other);
            }
            first // the winning handle is deliberately kept open for the life of the process
        };
        if first || std::time::Instant::now() >= deadline {
            return first;
        }
        std::thread::sleep(RESTART_POLL);
    }
}

fn safe_dll_search() {
    // SAFETY: a process-wide setting with no pointers.
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

fn describe(m: &xiro_input::Monitor) -> String {
    let tags: Vec<&str> = [(m.is_primary, "primary"), (m.is_virtual, "virtual")].iter().filter(|t| t.0).map(|t| t.1).collect();
    let tags = if tags.is_empty() { String::new() } else { format!("  [{}]", tags.join(", ")) };
    format!("{}  {}x{} at ({},{})  {}{tags}", m.name, m.rect.width, m.rect.height, m.rect.left, m.rect.top, m.adapter)
}

fn configure_autostart(mode: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    match mode {
        "on" => {
            autostart::enable(&autostart::command(&exe))?;
            log::raw("Xiro will start silently when you sign in.");
        }
        "off" => log::raw(if autostart::disable() { "Autostart removed." } else { "Autostart was not enabled." }),
        _ => log::raw(&format!("Autostart is {}. Use --autostart on|off.", if autostart::entry().is_some() { "on" } else { "off" })),
    }
    Ok(())
}

/// Help, autostart, elevated steps, uninstall and display listing. True when the process is done.
fn run_command(cli: &cli::Cli, exe: &Path) -> Result<bool, String> {
    if cli.setup_system || cli.remove_system {
        // Elevated, from a folder others may write to: load DLLs from System32 only.
        safe_dll_search();
    }
    if cli.setup_system {
        setup::setup_system(exe, &cli.ports, &cli.screens)?;
    } else if cli.remove_system {
        setup::remove_system()?;
    } else if cli.uninstall {
        // Not under a running Xiro: it would keep the driver busy and its files open.
        if first_instance(false) {
            desk::uninstall(exe);
        } else {
            install::message(TITLE, "Quit Xiro first (its tray icon \u{2192} Quit Xiro), then run the uninstall again.", false);
        }
    } else if cli.list_displays {
        for (i, m) in xiro_input::list_monitors().iter().enumerate() {
            log::raw(&format!("  {}. {}", i + 1, describe(m)));
        }
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// Offers to install or repair the display driver when the second screen can't work.
fn check_display(cli: &mut cli::Cli, config: &config::Config, exe: &Path) {
    let wanted = cli.display.clone().unwrap_or_else(|| config.display.clone()) == config::DisplayChoice::Auto;
    if !wanted || cli.background || desk::setup_postponed(exe) {
        return;
    }
    let ports = [config.http_port, config.https_port];
    if let Some(need) = setup::setup_need(exe, &ports) {
        cli.wait_for_driver = desk::offer_setup(need, &ports);
    }
}

/// Status line written by the hub and shown in the tray, plus notifications to the user.
fn callbacks(gui: bool) -> (Arc<Mutex<Status>>, app::Callbacks) {
    let status = Arc::new(Mutex::new(Status { line: "Starting\u{2026}".into() }));
    let tray_status = status.clone();
    let on_status = Arc::new(move |line: String| {
        if let Ok(mut s) = tray_status.lock() {
            s.line = line;
        }
    });
    // The newest device takes control of the PC: the person at the PC should know.
    let on_join: Arc<dyn Fn(&str) + Send + Sync> = if gui {
        Arc::new(|who: &str| xiro_shell::notify(TITLE, &format!("A device at {who} connected and now controls this PC.")))
    } else {
        Arc::new(|_: &str| {})
    };
    (status, app::Callbacks { on_status, on_join, approve: Arc::new(desk::approve_pairing) })
}

/// Runs until Ctrl+C, console close, sign-out or shutdown.
fn run_console(identity: &Identity, running: app::Running, detail: String) -> Result<(), String> {
    app::banner(identity, &[detail])?;
    let (stop, stopped) = mpsc::channel();
    let _ = STOP.set(stop);
    // SAFETY: registers a handler with the documented signature for the life of the process.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_event), true);
    }
    let _ = stopped.recv();
    running.shutdown();
    Ok(())
}

/// Starts the server, Xiro Wi-Fi and the tray.
fn serve(mut cli: cli::Cli, exe: &Path, dir: std::path::PathBuf, config: config::Config, identity: Arc<Identity>, first_run: bool) -> Result<(), String> {
    let gui = !cli.console;
    if gui {
        check_display(&mut cli, &config, exe);
    }
    let (status, callbacks) = callbacks(gui);
    let wifi = Arc::new(XiroWifi::load(&dir, &identity.hostname).map_err(|e| format!("Could not read the Xiro Wi-Fi settings: {e}"))?);
    let (running, detail) = app::start(&cli, dir.clone(), &config, &identity, callbacks)?;
    log::info!("Xiro {} is running. {detail}", env!("CARGO_PKG_VERSION"));
    if !gui {
        return run_console(&identity, running, detail);
    }
    let display_broken = desk::watch_display(dir.join("layout.json"));
    let ports = [config.http_port, config.https_port];
    let desk = desk::Desk { identity: &identity, wifi: &wifi, running: &running, display_broken, ports };
    let result = desk::run_tray(&desk, status, first_run || cli.restart);
    wifi.turn_off_for_exit();
    running.shutdown();
    result
}

fn run() -> Result<(), String> {
    let cli = cli::parse(std::env::args().skip(1))?;
    let console = attach_console();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cli.help {
        log::raw(cli::HELP);
        return Ok(());
    }
    if let Some(mode) = &cli.autostart {
        return configure_autostart(mode);
    }
    let dir = config::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    log::init(console && cli.console, Some(dir.join("xiro.log")));
    // DPI awareness must be set before any window, including the prompts.
    xiro_input::enable_dpi_awareness();
    if run_command(&cli, &exe)? {
        return Ok(());
    }
    let (config, warnings) = config::load(&dir);
    for warning in warnings {
        log::warning!("{warning}");
    }
    let first_run = !dir.join("token.txt").exists();
    let identity = Arc::new(Identity::load(&dir, &config, cli.new_token)?);
    if cli.pair {
        return app::banner(&identity, &[]);
    }
    if !first_instance(cli.restart) {
        if !cli.console && !cli.background {
            install::message(TITLE, "Xiro is already running. Find it in the taskbar tray (the ^ arrow next to the clock).", false);
        }
        return Ok(());
    }
    serve(cli, &exe, dir, config, identity, first_run)
}

fn main() {
    if let Err(err) = run() {
        log::error!("{err}");
        // Without a console the error would go unseen; elevated steps stay quiet (their parent reports).
        let elevated_step = std::env::args().any(|a| a == "--setup-system" || a == "--remove-system");
        if !elevated_step {
            install::message(TITLE, &err, true);
        }
        std::process::exit(1);
    }
}
