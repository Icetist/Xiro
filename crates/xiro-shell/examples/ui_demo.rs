//! Shows the tray and pairing window with sample data, saves a screenshot, then quits.
//! Usage: `cargo run -p xiro-shell --example ui_demo [screenshot.bmp]`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BitBlt, SRCCOPY, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDC, GetDIBits, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect, PW_RENDERFULLCONTENT, PostMessageW, WM_CLOSE};
use windows::core::{PCWSTR, w};
use xiro_shell::{DisplayHealth, MenuState, PairingInfo, Status, TrayAction, WifiInfo, WifiState};

const SHOW_AFTER: Duration = Duration::from_millis(1000);
const CAPTURE_AFTER: Duration = Duration::from_millis(1500);
const QUIT_AFTER: Duration = Duration::from_millis(2500);
/// Hard stop in case the tray never quits.
const WATCHDOG: Duration = Duration::from_secs(20);

fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is writable.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok().map(|()| rect)
}

fn save(path: &std::path::Path, image: Option<(i32, i32, Vec<u8>)>) {
    match image.map(|(w, h, pixels)| (w, h, write_bmp(path, w, h, &pixels))) {
        Some((w, h, Ok(()))) => println!("screenshot {w}x{h}: {}", path.display()),
        Some((_, _, Err(e))) => eprintln!("could not save {}: {e}", path.display()),
        None => eprintln!("nothing captured for {}", path.display()),
    }
}

fn find(class: PCWSTR) -> Option<HWND> {
    // SAFETY: plain lookup by class name.
    unsafe { FindWindowW(class, PCWSTR::null()) }.ok()
}

/// Where the shell shows our tray icon (fails if it is not on the taskbar).
fn tray_icon_rect(tray: HWND) -> Option<RECT> {
    let id = NOTIFYICONIDENTIFIER { cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32, hWnd: tray, uID: 1, ..Default::default() };
    // SAFETY: `id` is fully initialised.
    unsafe { Shell_NotifyIconGetRect(&id) }.ok()
}

/// Captures a window (PrintWindow) or screen area (BitBlt) as top-down BGRA: (w, h, rows).
fn capture(window: Option<HWND>, area: RECT) -> Option<(i32, i32, Vec<u8>)> {
    let (width, height) = (area.right - area.left, area.bottom - area.top);
    // SAFETY: GDI objects are created and released within this block.
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
        let printed = match window {
            Some(hwnd) => PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool(),
            None => BitBlt(memory, 0, 0, width, height, Some(screen), area.left, area.top, SRCCOPY).is_ok(),
        };
        SelectObject(memory, previous);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let rows = GetDIBits(memory, bitmap, 0, height as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        (printed && rows == height).then_some((width, height, pixels))
    }
}

/// Top-down BMP: negative height in BITMAPINFOHEADER.
fn write_bmp(path: &std::path::Path, width: i32, height: i32, pixels: &[u8]) -> std::io::Result<()> {
    const HEADERS: u32 = 14 + 40;
    let mut out = Vec::with_capacity(HEADERS as usize + pixels.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(HEADERS + pixels.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&HEADERS.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&(-height).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]); // BI_RGB, sizes and palette fields left zero
    out.extend_from_slice(pixels);
    std::fs::write(path, out)
}

fn main() {
    // SAFETY: process-wide DPI mode, set before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let screenshot = std::env::args().nth(1).filter(|a| a.ends_with(".bmp")).map_or_else(|| std::env::temp_dir().join("xiro-pairing.bmp"), Into::into);
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        eprintln!("watchdog: tray did not quit");
        std::process::exit(2);
    });
    std::thread::spawn(move || {
        std::thread::sleep(SHOW_AFTER);
        if std::env::args().any(|a| a == "prompt") {
            // `ui_demo prompt` shows the repair dialog instead of the window.
            std::thread::spawn(|| {
                xiro_shell::ask(xiro_shell::Prompt {
                    caption: "Xiro",
                    badge: xiro_shell::Badge::Shield,
                    title: "Fix the second screen".into(),
                    body: "Xiro will set its display driver up for your devices and restart it. Windows asks for permission, then Xiro restarts.".into(),
                    detail: None,
                    buttons: vec![("Not now".into(), xiro_shell::Choice::Plain), ("Fix".into(), xiro_shell::Choice::Main)],
                    default: 1,
                    cancel: 0,
                    arm: None,
                    timeout: Some(Duration::from_secs(30)),
                    topmost: true,
                })
            });
        } else {
            xiro_shell::show_pairing();
            xiro_shell::notify("Xiro demo", "The tray icon works. This demo closes in a few seconds.");
        }
        std::thread::sleep(CAPTURE_AFTER);
        let pairing = find(w!("XiroWindow"));
        save(&screenshot, pairing.and_then(|p| capture(Some(p), window_rect(p)?)));
        let tray = find(w!("XiroTrayWindow"));
        match tray.and_then(tray_icon_rect) {
            Some(rect) => save(&screenshot.with_extension("tray.bmp"), capture(None, rect)),
            None => eprintln!("tray icon not visible on the taskbar (may be in the overflow area)"),
        }
        std::thread::sleep(QUIT_AFTER);
        if let Some(tray) = tray {
            // SAFETY: asking our own tray window to close, which ends `run_tray`.
            let _ = unsafe { PostMessageW(Some(tray), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
    });

    let info = PairingInfo {
        setup_url: "http://192.168.1.23:8080/#t=Zm9vYmFyYmF6cXV4&p=8443&h=DEMO-PC.local".into(),
        app_url: "https://192.168.1.23:8443/#t=Zm9vYmFyYmF6cXV4".into(),
        fingerprint: "3F:A2:91:0C:5B:77:E4:12:9D:C8:40:6A:BB:13:F0:2E:85:D9:61:4C:A7:3E:08:F5:92:6B:1D:CC:E0:57:A4:39".into(),
        addresses: vec!["10.0.0.5".into(), "172.20.10.2".into(), "DEMO-PC.local".into()],
        // `ui_demo wifi` shows the two-code layout used while Xiro Wi-Fi is on.
        wifi: std::env::args().any(|a| a == "wifi").then(|| WifiInfo {
            ssid: "Xiro-DEMO-PC".into(),
            passphrase: "k7Hq2mXp9RtW4vNc".into(),
            qr_text: "WIFI:T:WPA;S:Xiro-DEMO-PC;P:k7Hq2mXp9RtW4vNc;;".into(),
        }),
    };
    // `ui_demo waiting` shows the window before any device connected.
    let waiting = std::env::args().any(|a| a == "waiting");
    let line = if waiting { "Waiting for an iPad or iPhone" } else { "1 device connected \u{b7} 2360\u{d7}1640 \u{b7} 60 fps" };
    let status = Arc::new(Mutex::new(Status { line: line.into() }));
    // `ui_demo broken` shows the "fix the second screen" card.
    let state = MenuState {
        autostart: true,
        xiro_wifi: if info.wifi.is_some() { WifiState::On } else { WifiState::Off },
        display: if std::env::args().any(|a| a == "broken") { DisplayHealth::NeedsRepair } else { DisplayHealth::Ready },
    };
    let result = xiro_shell::run_tray(move || info.clone(), status, move || state, |action: TrayAction| println!("action: {action:?}"));
    println!("tray exited: {result:?}");
}
