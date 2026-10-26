//! The custom-drawn window behind every Xiro view: Direct2D target, input and springs.
//! The system title bar is kept, coloured to match the content.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWINDOWATTRIBUTE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_RETURN, VK_SHIFT,
    VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HICON, HTCLIENT,
    ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, RegisterClassExW, SM_CXICON, SM_CXSMICON,
    SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetCursor, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, SystemParametersInfoW, PostMessageW, WM_APP, WM_CAPTURECHANGED, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SETICON,
    WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_EX_TOPMOST, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::{PCWSTR, w};

use windows::Win32::UI::Controls::WM_MOUSELEAVE;

use super::controls::{self, Channel, Control, Motion, Pointer};
use super::gfx::{Gfx, Target};
use super::paint::Painter;
use super::theme::{self, Palette};
use crate::icon::{OwnedIcon, create_icon};
use crate::wide::to_wide;

const CLASS_NAME: PCWSTR = w!("XiroWindow");
const ANIMATION_TIMER: usize = 1;
const VIEW_TIMER: usize = 2;
const FRAME_MS: u32 = 16;
const BASE_DPI: f32 = 96.0;
/// DWMWCP_ROUND.
const ROUND_CORNERS: u32 = 2;
/// Posted by `apply(Close)`: destroy the window from the message loop.
const WM_DESTROY_LATER: u32 = WM_APP + 0x58;

/// What the window should do after the view handled something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Nothing,
    Repaint,
    Hide,
    Close,
}

/// The content of a window.
pub(crate) trait View {
    /// Client size in DIPs.
    fn size(&self, gfx: &Gfx) -> (f32, f32);
    fn controls(&self) -> Vec<Control>;
    fn paint(&self, painter: &Painter, size: (f32, f32));
    fn activate(&mut self, id: u16) -> Reply;
    /// Esc or the close button.
    fn dismiss(&mut self) -> Reply;
    /// Enter, when no control has keyboard focus.
    fn default_control(&self) -> Option<u16> {
        None
    }
    /// How often `tick` should run, if at all.
    fn tick_every(&self) -> Option<Duration> {
        None
    }
    fn tick(&mut self, _now: Instant) -> Reply {
        Reply::Nothing
    }
    /// Enter always means `default_control`, so a risky button needs a deliberate click or Space.
    fn enter_means_default(&self) -> bool {
        false
    }
    /// For owners that update their own kind of view through [`update`].
    fn as_any(&mut self) -> &mut dyn std::any::Any;
}

pub(crate) struct Options {
    pub title: &'static str,
    pub topmost: bool,
    /// A dialog: only the close button in the title bar.
    pub dialog: bool,
}

struct Window {
    view: RefCell<Box<dyn View>>,
    gfx: Gfx,
    target: RefCell<Option<Target>>,
    motion: RefCell<Motion>,
    pointer: Cell<Pointer>,
    palette: Cell<&'static Palette>,
    last_frame: Cell<Instant>,
    icons: RefCell<Vec<OwnedIcon>>,
    style: WINDOW_STYLE,
}

fn instance() -> HINSTANCE {
    // SAFETY: this module's own handle.
    unsafe { GetModuleHandleW(None) }.map(|m| HINSTANCE(m.0)).unwrap_or_default()
}

fn register() {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance(),
        // SAFETY: a stock cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: fully initialised; registering twice fails harmlessly.
    unsafe { RegisterClassExW(&class) };
}

fn set_attribute<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) {
    // SAFETY: `value` matches the size passed; unknown attributes just fail on older Windows.
    let _ = unsafe { DwmSetWindowAttribute(hwnd, attribute, (value as *const T).cast(), size_of::<T>() as u32) };
}

/// Title bar and border in the window's own colours, rounded corners, dark mode.
fn style_frame(hwnd: HWND, p: &Palette) {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &u32::from(p.dark));
    set_attribute(hwnd, DWMWA_CAPTION_COLOR, &p.ink.colorref());
    set_attribute(hwnd, DWMWA_TEXT_COLOR, &p.secondary.colorref());
    set_attribute(hwnd, DWMWA_BORDER_COLOR, &p.ink.mix(p.text, 0.12).colorref());
    set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &ROUND_CORNERS);
}

fn styles(options: &Options) -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    let minimize = if options.dialog { WINDOW_STYLE(0) } else { WS_MINIMIZEBOX };
    (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | minimize, if options.topmost { WS_EX_TOPMOST } else { WINDOW_EX_STYLE(0) })
}

/// Creates the (hidden) window for `view`. Show it with `ShowWindow`.
pub(crate) fn create(view: Box<dyn View>, options: &Options) -> Result<HWND, String> {
    register();
    let gfx = Gfx::new()?;
    let (style, ex_style) = styles(options);
    let title = to_wide(options.title);
    // SAFETY: registered class; strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(ex_style, CLASS_NAME, PCWSTR(title.as_ptr()), style, 0, 0, 100, 100, None, None, Some(instance()), None)
    }
    .map_err(|e| format!("could not create a window: {}", e.message()))?;
    let palette = theme::current();
    let state = Box::new(Window {
        view: RefCell::new(view),
        gfx,
        target: RefCell::new(None),
        motion: RefCell::new(Motion::default()),
        pointer: Cell::new(Pointer::default()),
        palette: Cell::new(palette),
        last_frame: Cell::new(Instant::now()),
        icons: RefCell::new(Vec::new()),
        style,
    });
    // SAFETY: from here the window owns the state; freed in WM_NCDESTROY.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize) };
    style_frame(hwnd, palette);
    if let Some(window) = window_of(hwnd) {
        set_icons(hwnd, window);
        place(hwnd, window, None); // sizes the view first: its controls depend on it
        sync_motion(window);
        start_view_timer(hwnd, window);
    }
    Ok(hwnd)
}

fn window_of<'a>(hwnd: HWND) -> Option<&'a Window> {
    // SAFETY: USERDATA is 0 or the `Window` installed by `create`, alive until WM_NCDESTROY.
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Window).as_ref() }
}

fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: plain query; 0 for an invalid window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    // SAFETY: plain query.
    if dpi == 0 { unsafe { GetDpiForSystem() } } else { dpi }
}

fn set_icons(hwnd: HWND, window: &Window) {
    let dpi = dpi_of(hwnd);
    // SAFETY: plain metrics queries.
    let (big, small) = unsafe { (GetSystemMetricsForDpi(SM_CXICON, dpi), GetSystemMetricsForDpi(SM_CXSMICON, dpi)) };
    let (Ok(big), Ok(small)) = (create_icon(big as u32), create_icon(small as u32)) else { return };
    let send = |kind: u32, icon: HICON| {
        // SAFETY: WM_SETICON with an icon kept alive in `window.icons`.
        unsafe { SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize))) };
    };
    send(ICON_BIG, big.0);
    send(ICON_SMALL, small.0);
    *window.icons.borrow_mut() = vec![big, small];
}

/// Sizes the window to its view at its DPI; centred in the work area unless `at` is given.
fn place(hwnd: HWND, window: &Window, at: Option<(i32, i32)>) {
    let dpi = dpi_of(hwnd);
    let (w, h) = window.view.borrow().size(&window.gfx);
    let scale = dpi as f32 / BASE_DPI;
    let mut frame = RECT { left: 0, top: 0, right: (w * scale).round() as i32, bottom: (h * scale).round() as i32 };
    // SAFETY: `frame` is a valid RECT.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut frame, window.style, false, WINDOW_EX_STYLE(0), dpi) };
    let (outer_w, outer_h) = (frame.right - frame.left, frame.bottom - frame.top);
    let (x, y) = at.unwrap_or_else(|| {
        let mut work = RECT::default();
        // SAFETY: SPI_GETWORKAREA fills a RECT.
        let _ = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, Some((&mut work as *mut RECT).cast()), Default::default()) };
        (work.left + (work.right - work.left - outer_w) / 2, (work.top + (work.bottom - work.top - outer_h) / 2).max(work.top))
    });
    // SAFETY: repositioning our own window.
    let _ = unsafe { SetWindowPos(hwnd, None, x, y, outer_w, outer_h, SWP_NOZORDER | SWP_NOACTIVATE) };
}

fn start_view_timer(hwnd: HWND, window: &Window) {
    if let Some(every) = window.view.borrow().tick_every() {
        // SAFETY: a timer on our own window.
        unsafe { SetTimer(Some(hwnd), VIEW_TIMER, every.as_millis().clamp(10, 60_000) as u32, None) };
    }
}

/// Points the springs at the current pointer and switch states.
fn sync_motion(window: &Window) {
    let pointer = window.pointer.get();
    let controls = window.view.borrow().controls();
    let mut motion = window.motion.borrow_mut();
    let level = |b: bool| if b { 1.0 } else { 0.0 };
    for c in &controls {
        motion.set((c.id, Channel::Hover), level(pointer.hover == Some(c.id) && c.enabled));
        motion.set((c.id, Channel::Press), level(pointer.pressed == Some(c.id) && pointer.hover == Some(c.id)));
        motion.set((c.id, Channel::Switch), level(c.on));
    }
}

fn changed(hwnd: HWND, window: &Window) {
    sync_motion(window);
    window.last_frame.set(Instant::now());
    // SAFETY: timers and invalidation on our own window.
    unsafe {
        SetTimer(Some(hwnd), ANIMATION_TIMER, FRAME_MS, None);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn client_px(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    // SAFETY: valid out-parameter.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    ((rc.right - rc.left).max(1) as u32, (rc.bottom - rc.top).max(1) as u32)
}

fn paint(hwnd: HWND, window: &Window) {
    let dpi = dpi_of(hwnd);
    let (w, h) = client_px(hwnd);
    if window.target.borrow().is_none() {
        *window.target.borrow_mut() = window.gfx.target(hwnd, dpi, w, h).ok();
    }
    let ok = {
        let target = window.target.borrow();
        let Some(target) = target.as_ref() else { return };
        let motion = window.motion.borrow();
        let painter = Painter { t: target, g: &window.gfx, p: window.palette.get(), motion: &motion, pointer: window.pointer.get() };
        let scale = dpi as f32 / BASE_DPI;
        let view = window.view.borrow();
        target.frame(|t| {
            t.clear(painter.p.ink);
            view.paint(&painter, (w as f32 / scale, h as f32 / scale));
        })
    };
    if !ok {
        *window.target.borrow_mut() = None; // the GPU changed: rebuilt on the next paint
        // SAFETY: asks for that next paint.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }
}

fn apply(hwnd: HWND, window: &Window, reply: Reply) {
    match reply {
        Reply::Nothing => {}
        Reply::Repaint => changed(hwnd, window),
        Reply::Hide => {
            // SAFETY: hiding our own window.
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
        Reply::Close => {
            // Destroyed from the message loop, never while this handler's `&Window` is alive.
            // SAFETY: posting to our own window.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_DESTROY_LATER, WPARAM(0), LPARAM(0)) };
        }
    }
}

fn activate(hwnd: HWND, window: &Window, id: u16) {
    let reply = window.view.borrow_mut().activate(id);
    apply(hwnd, window, reply);
    if reply == Reply::Nothing {
        changed(hwnd, window); // a click still moves the springs (press released)
    }
}

fn point(hwnd: HWND, lparam: LPARAM) -> (f32, f32) {
    let scale = dpi_of(hwnd) as f32 / BASE_DPI;
    let x = f32::from(lparam.0 as u16 as i16);
    let y = f32::from((lparam.0 >> 16) as u16 as i16);
    (x / scale, y / scale)
}

fn on_mouse(hwnd: HWND, window: &Window, msg: u32, lparam: LPARAM) {
    let (x, y) = point(hwnd, lparam);
    let over = controls::hit(&window.view.borrow().controls(), x, y);
    let before = window.pointer.get();
    let mut pointer = before;
    match msg {
        WM_MOUSEMOVE => {
            if pointer.hover.is_none() {
                let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                // SAFETY: asks for one WM_MOUSELEAVE.
                let _ = unsafe { TrackMouseEvent(&mut track) };
            }
            pointer.hover = over;
        }
        WM_LBUTTONDOWN => {
            pointer.pressed = over;
            pointer.focus = over.or(pointer.focus);
            pointer.keyboard = false;
            // SAFETY: capture so the release is seen even outside the window.
            unsafe { SetCapture(hwnd) };
        }
        WM_LBUTTONUP => {
            // SAFETY: ends the capture taken on button down.
            let _ = unsafe { ReleaseCapture() };
            let clicked = pointer.pressed.filter(|p| Some(*p) == over);
            pointer.pressed = None;
            window.pointer.set(pointer);
            if let Some(id) = clicked {
                activate(hwnd, window, id);
                return;
            }
        }
        _ => pointer.hover = None, // WM_MOUSELEAVE
    }
    window.pointer.set(pointer);
    if before != pointer {
        changed(hwnd, window);
    }
}

fn on_key(hwnd: HWND, window: &Window, key: u16) -> bool {
    let mut pointer = window.pointer.get();
    if key == VK_TAB.0 {
        // SAFETY: plain key-state query; negative means "down".
        let back = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
        pointer.focus = controls::next_focus(&window.view.borrow().controls(), pointer.focus, back);
        pointer.keyboard = true;
        window.pointer.set(pointer);
        changed(hwnd, window);
    } else if key == VK_RETURN.0 || key == VK_SPACE.0 {
        let enter_default = key == VK_RETURN.0 && window.view.borrow().enter_means_default();
        let focused = if pointer.keyboard && !enter_default { pointer.focus } else { None };
        let fallback = if key == VK_RETURN.0 { window.view.borrow().default_control() } else { None };
        let enabled = |id: &u16| window.view.borrow().controls().iter().any(|c| c.id == *id && c.enabled);
        if let Some(id) = focused.or(fallback).filter(enabled) {
            activate(hwnd, window, id);
        }
    } else if key == VK_ESCAPE.0 {
        let reply = window.view.borrow_mut().dismiss();
        apply(hwnd, window, reply);
    } else {
        return false;
    }
    true
}

fn on_timer(hwnd: HWND, window: &Window, id: usize) {
    if id == ANIMATION_TIMER {
        let now = Instant::now();
        let dt = now.duration_since(window.last_frame.replace(now)).as_secs_f32();
        if !window.motion.borrow_mut().step(dt) {
            // SAFETY: stops our own timer once everything settled.
            let _ = unsafe { KillTimer(Some(hwnd), ANIMATION_TIMER) };
        }
        // SAFETY: invalidating our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    } else if id == VIEW_TIMER {
        let reply = window.view.borrow_mut().tick(Instant::now());
        apply(hwnd, window, reply);
    }
}

fn on_dpi(hwnd: HWND, window: &Window, lparam: LPARAM) {
    // SAFETY: for WM_DPICHANGED, lParam points to the suggested window RECT.
    let suggested = unsafe { *(lparam.0 as *const RECT) };
    if let Some(target) = window.target.borrow_mut().as_mut() {
        target.set_dpi(dpi_of(hwnd));
    }
    set_icons(hwnd, window);
    place(hwnd, window, Some((suggested.left, suggested.top)));
}

fn handle(hwnd: HWND, window: &Window, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_PAINT => {
            paint(hwnd, window);
            // SAFETY: the whole client area is now drawn.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
        }
        WM_ERASEBKGND => return Some(LRESULT(1)), // Direct2D paints every pixel
        WM_SIZE => {
            let (w, h) = client_px(hwnd);
            if let Some(target) = window.target.borrow().as_ref() {
                target.resize(w, h);
            }
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP => on_mouse(hwnd, window, msg, lparam),
        WM_KEYDOWN if on_key(hwnd, window, wparam.0 as u16) => {}
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT && window.pointer.get().hover.is_some() => {
            // SAFETY: a stock cursor and a plain setter.
            unsafe { SetCursor(LoadCursorW(None, IDC_HAND).ok()) };
            return Some(LRESULT(1));
        }
        WM_TIMER => on_timer(hwnd, window, wparam.0),
        WM_CAPTURECHANGED => {
            let mut pointer = window.pointer.get();
            if pointer.pressed.take().is_some() {
                window.pointer.set(pointer);
                changed(hwnd, window);
            }
        }
        WM_DPICHANGED => on_dpi(hwnd, window, lparam),
        WM_SETTINGCHANGE => {
            window.palette.set(theme::current());
            style_frame(hwnd, window.palette.get());
            changed(hwnd, window);
        }
        WM_CLOSE => {
            let reply = window.view.borrow_mut().dismiss();
            apply(hwnd, window, reply);
        }
        _ => return None,
    }
    Some(LRESULT(0))
}

/// Lets the owner of the window tell its view something changed (e.g. new pairing info).
pub(crate) fn update(hwnd: HWND, change: impl FnOnce(&mut dyn View) -> Reply) {
    let Some(window) = window_of(hwnd) else { return };
    let reply = {
        let Ok(mut view) = window.view.try_borrow_mut() else { return };
        change(view.as_mut())
    };
    apply(hwnd, window, reply);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY_LATER {
        // SAFETY: our own window; no reference to its state is alive at this point.
        let _ = unsafe { DestroyWindow(hwnd) };
        return LRESULT(0);
    }
    if msg == WM_NCDESTROY {
        // SAFETY: the last message: take back and free the state installed by `create`.
        unsafe {
            let _ = KillTimer(Some(hwnd), ANIMATION_TIMER);
            let _ = KillTimer(Some(hwnd), VIEW_TIMER);
            let raw = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Window;
            if !raw.is_null() {
                drop(Box::from_raw(raw));
            }
        }
    } else if let Some(window) = window_of(hwnd) {
        if let Some(result) = handle(hwnd, window, msg, wparam, lparam) {
            return result;
        }
    }
    // SAFETY: default processing for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
