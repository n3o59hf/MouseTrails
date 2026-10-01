//! Fullscreen transparent click-through overlay (layered window) + tray icon.
//!
//! Effects are rendered with tiny-skia into a 32-bit DIB that backs the whole
//! virtual screen; only the dirty rectangle is pushed to the screen each frame
//! via UpdateLayeredWindowIndirect. The DIB memory is handed to tiny-skia
//! directly, so there are no copies in the render path.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tiny_skia::PixmapMut;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::effects::{self, Effects};
use crate::updater;
use crate::settings::SharedSettings;
use crate::settings_ui::SettingsUiManager;
use crate::startup;

const TIMER_ID: usize = 1;
const TRAY_ID: u32 = 1;
const WM_APP_TRAY: u32 = WM_APP + 1;
const ID_SETTINGS: u32 = 1;
const ID_STARTUP: u32 = 2;
const ID_EXIT: u32 = 3;
const ID_UPDATE_CHECK: u32 = 4;
const ID_UPDATE_INSTALL: u32 = 5;

// Diagnostics, enabled via MOUSETRAILS_DEBUG / MOUSETRAILS_DUMP env vars.
pub(crate) fn debug_log(line: String) {
    let path = std::env::temp_dir().join("mousetrails_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// High-frequency cursor sampler: a background thread polls GetCursorPos
/// ~every millisecond so the trail follows the *real* cursor path (including
/// direction changes between render frames) instead of 60 Hz straight lines.
struct Sampler {
    stop: Arc<AtomicBool>,
    buf: Arc<Mutex<Vec<(i32, i32, f32)>>>, // screen x, y, seconds since start
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start(t0: Instant) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let stop2 = stop.clone();
        let buf2 = buf.clone();
        let thread = std::thread::Builder::new()
            .name("cursor-sampler".into())
            .spawn(move || {
                unsafe {
                    let _ = timeBeginPeriod(1); // make Sleep(1) actually ~1ms
                    let mut last = (i32::MIN, i32::MIN);
                    loop {
                        if stop2.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut pt = POINT::default();
                        if GetCursorPos(&mut pt).is_ok() && (pt.x, pt.y) != last {
                            last = (pt.x, pt.y);
                            let t = t0.elapsed().as_secs_f32();
                            let mut b = buf2.lock().unwrap();
                            if b.len() > 8192 {
                                b.drain(..4096); // render stalled — drop history
                            }
                            b.push((pt.x, pt.y, t));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    let _ = timeEndPeriod(1);
                }
            })
            .ok();
        Self { stop, buf, thread }
    }

    /// Take everything sampled since the last call (screen coordinates).
    fn drain(&self) -> Vec<(i32, i32, f32)> {
        std::mem::take(&mut *self.buf.lock().unwrap())
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

thread_local! {
    static APP: RefCell<Option<Box<App>>> = const { RefCell::new(None) };
}

enum TrayAction {
    None,
    ShowMenu,
    Exit,
}

pub fn run(shared: Arc<SharedSettings>, open_settings_at_start: bool) {
    effects::seed_rng();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    APP.with(|slot| *slot.borrow_mut() = Some(Box::new(App::new(shared))));

    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).expect("GetModuleHandleW").into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("MouseTrailsOverlay"),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);

        let (vx, vy, vw, vh) = virtual_screen();
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("MouseTrailsOverlay"),
            w!("MouseTrails"),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("CreateWindowExW");

        // Show first, then establish the layered surface — a surface pushed
        // before the window is shown is ignored by DWM afterwards on some
        // Windows builds.
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );

        APP.with(|slot| {
            let mut guard = slot.borrow_mut();
            let app = guard.as_mut().unwrap();
            app.after_create(hwnd);
        });

        if open_settings_at_start {
            APP.with(|slot| {
                let mut app = slot.borrow_mut();
                let app = app.as_mut().unwrap();
                app.ui.open(&app.shared);
            });
        }

        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                break; // WM_QUIT (0) or error (-1)
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        APP.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            APP.with(|slot| {
                if let Ok(mut guard) = slot.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.tick();
                    }
                }
            });
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let action = APP.with(|slot| {
                if let Ok(mut guard) = slot.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        return app.tray_event(lparam);
                    }
                }
                TrayAction::None
            });
            if matches!(action, TrayAction::ShowMenu) {
                show_tray_menu();
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let action = APP.with(|slot| {
                if let Ok(mut guard) = slot.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        return app.command(wparam);
                    }
                }
                TrayAction::None
            });
            if matches!(action, TrayAction::Exit) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            APP.with(|slot| {
                if let Ok(mut guard) = slot.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.on_display_change();
                    }
                }
            });
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
            APP.with(|slot| {
                if let Ok(mut guard) = slot.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.on_destroy();
                    }
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_tray_menu() {
    let (hwnd, update_available) = APP.with(|slot| {
        let guard = slot.borrow();
        match guard.as_ref() {
            Some(app) => {
                let sha = app.shared.update.lock().unwrap().available_sha.clone();
                (app.hwnd, sha)
            }
            None => (HWND::default(), None),
        }
    });
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        // Rebuild per show so the update items reflect the current state.
        let menu = CreatePopupMenu().expect("CreatePopupMenu");
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS as usize, w!("Open Settings..."));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        if let Some(sha) = &update_available {
            let label = updater::wide(&format!("Install update ({sha})"));
            let _ = AppendMenuW(menu, MF_STRING, ID_UPDATE_INSTALL as usize, PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_STRING, ID_UPDATE_CHECK as usize, w!("Check for updates"));
        let _ = AppendMenuW(menu, MF_STRING, ID_STARTUP as usize, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT as usize, w!("Exit"));
        let _ = SetMenuDefaultItem(menu, ID_SETTINGS, 0);
        let check = if startup::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        let _ = CheckMenuItem(menu, ID_STARTUP, (MF_BYCOMMAND | check).0);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    }
}

/// Background worker: checks GitHub on startup and then every 6 hours,
/// updating the shared status for the tray and settings window.
fn spawn_auto_check(shared: Arc<SharedSettings>) {
    std::thread::Builder::new()
        .name("update-autocheck".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(90));
            loop {
                match updater::check_remote() {
                    Ok(remote) => {
                        updater::record_check(&shared, &remote);
                    }
                    Err(e) => {
                        shared.update.lock().unwrap().status = format!("Update check failed: {e}");
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
            }
        })
        .ok();
}

/// "Check for updates" from the tray: runs off the UI thread and reports via
/// a message box, offering to install when an update is available.
fn check_and_prompt(shared: Arc<SharedSettings>) {
    let result = updater::check_remote();
    let (text, flags, offer_install) = match &result {
        Ok(remote) => {
            let available = updater::record_check(&shared, remote);
            if available {
                (
                    format!(
                        "Update available: build {} ({}).\n\nInstall and restart MouseTrails now?",
                        remote.sha, remote.date
                    ),
                    MB_YESNO | MB_ICONQUESTION,
                    true,
                )
            } else {
                (
                    format!(
                        "MouseTrails is up to date (build {} from {}).\n\nRemote: build {} ({}).",
                        updater::build_sha(),
                        updater::build_date(),
                        remote.sha,
                        remote.date
                    ),
                    MB_OK | MB_ICONINFORMATION,
                    false,
                )
            }
        }
        Err(e) => (
            format!("Update check failed:\n{e}"),
            MB_OK | MB_ICONWARNING,
            false,
        ),
    };
    let wide = updater::wide(&text);
    unsafe {
        let choice = MessageBoxW(
            HWND::default(),
            PCWSTR(wide.as_ptr()),
            w!("MouseTrails"),
            flags | MB_SETFOREGROUND | MB_TOPMOST,
        );
        if offer_install && choice.0 == 6 {
            crate::updater::spawn_install(shared);
        }
    }
}

struct App {
    shared: Arc<SharedSettings>,
    ui: SettingsUiManager,

    hwnd: HWND,
    hdc_mem: HDC,
    hbmp: HBITMAP,
    old_obj: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
    vx: i32,
    vy: i32,

    effects: Effects,
    prev_bounds: Option<effects::RectF>,
    last_tick: Instant,
    clock: f32,
    timer_period: u32,
    metric_checks: u32,
    last_topmost_assert: Instant,

    mouse: (f32, f32),
    vel: (f32, f32),
    prev_left: bool,

    hicon: HICON,

    dump_counter: u32,
    t0: Instant,
    sampler: Option<Sampler>,
}

impl App {
    fn new(shared: Arc<SharedSettings>) -> Self {
        Self {
            shared,
            ui: SettingsUiManager::new(),
            hwnd: HWND::default(),
            hdc_mem: HDC::default(),
            hbmp: HBITMAP::default(),
            old_obj: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            w: 0,
            h: 0,
            vx: 0,
            vy: 0,
            effects: Effects::new(),
            prev_bounds: None,
            last_tick: Instant::now(),
            clock: 0.0,
            timer_period: 15,
            metric_checks: 0,
            last_topmost_assert: Instant::now(),
            mouse: (0.0, 0.0),
            vel: (0.0, 0.0),
            prev_left: false,
            hicon: HICON::default(),
            dump_counter: 0,
            t0: Instant::now(),
            sampler: None,
        }
    }

    fn after_create(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        self.load_buffers();
        self.create_icon();
        self.add_tray_icon();
        self.sampler = Some(Sampler::start(self.t0));
        self.shared
            .overlay_hwnd
            .store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        spawn_auto_check(self.shared.clone());
        unsafe {
            SetTimer(self.hwnd, TIMER_ID, self.timer_period, None);
        }
    }

    /// (Re)creates the memory DC + full-virtual-screen DIB.
    fn load_buffers(&mut self) {
        unsafe {
            let (vx, vy, vw, vh) = virtual_screen();
            self.vx = vx;
            self.vy = vy;
            self.w = vw;
            self.h = vh;

            if self.hdc_mem.is_invalid() {
                self.hdc_mem = CreateCompatibleDC(HDC::default());
            }

            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = vw;
            bmi.bmiHeader.biHeight = -vh; // top-down
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;
            let hbmp = CreateDIBSection(
                HDC::default(),
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE::default(),
                0,
            )
            .expect("CreateDIBSection");

            self.old_obj = SelectObject(self.hdc_mem, HGDIOBJ(hbmp.0));
            if !self.hbmp.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.hbmp.0));
            }
            self.hbmp = hbmp;
            self.bits = bits as *mut u8;
            self.prev_bounds = None;

            // Start from a fully transparent surface and push one full update
            // so DWM has a valid backing store.
            let len = (vw as usize) * (vh as usize) * 4;
            std::slice::from_raw_parts_mut(self.bits, len).fill(0);
            self.update_layered(RECT { left: 0, top: 0, right: vw, bottom: vh });
        }
    }

    fn create_icon(&mut self) {
        unsafe {
            let bgra = crate::icon::icon_bgra_premul();
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = 32;
            bmi.bmiHeader.biHeight = -32;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;
            let color = CreateDIBSection(
                HDC::default(),
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE::default(),
                0,
            )
            .expect("icon DIB");
            std::slice::from_raw_parts_mut(bits as *mut u8, 32 * 32 * 4).copy_from_slice(&bgra);

            let mask_bits = [0u8; 32 * 4]; // 1bpp, 32 rows padded to 4 bytes
            let mask = CreateBitmap(32, 32, 1, 1, Some(mask_bits.as_ptr() as *const _));
            let ii = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&ii).expect("CreateIconIndirect");
            let _ = DeleteObject(HGDIOBJ(color.0));
            let _ = DeleteObject(HGDIOBJ(mask.0));
            self.hicon = icon;
        }
    }

    fn add_tray_icon(&mut self) {
        unsafe {
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = TRAY_ID;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            nid.uCallbackMessage = WM_APP_TRAY;
            nid.hIcon = self.hicon.into();
            let tip: Vec<u16> = "MouseTrails\0".encode_utf16().collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
    }

    fn tray_event(&mut self, lparam: LPARAM) -> TrayAction {
        match lparam.0 as u32 {
            WM_RBUTTONUP => TrayAction::ShowMenu,
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                self.ui.open(&self.shared);
                TrayAction::None
            }
            _ => TrayAction::None,
        }
    }

    fn command(&mut self, wparam: WPARAM) -> TrayAction {
        let id = (wparam.0 & 0xFFFF) as u32;
        match id {
            ID_SETTINGS => {
                self.ui.open(&self.shared);
                TrayAction::None
            }
            ID_UPDATE_CHECK => {
                let shared = self.shared.clone();
                std::thread::Builder::new()
                    .name("update-check".into())
                    .spawn(move || check_and_prompt(shared))
                    .ok();
                TrayAction::None
            }
            ID_UPDATE_INSTALL => {
                updater::spawn_install(self.shared.clone());
                TrayAction::None
            }
            ID_STARTUP => {
                let target = !startup::is_enabled();
                let res = startup::set_enabled(target);
                if res.is_ok() {
                    self.ui.notify_startup_changed(target);
                }
                TrayAction::None
            }
            ID_EXIT => TrayAction::Exit,
            _ => TrayAction::None,
        }
    }

    fn on_display_change(&mut self) {
        let m = virtual_screen();
        if m != (self.vx, self.vy, self.w, self.h) {
            unsafe {
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, m.0, m.1, m.2, m.3, SWP_NOACTIVATE);
            }
            self.load_buffers();
        }
    }

    fn on_destroy(&mut self) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_ID);
            let mut nid = NOTIFYICONDATAW::default();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = self.hwnd;
            nid.uID = TRAY_ID;
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            if !self.hdc_mem.is_invalid() {
                let _ = SelectObject(self.hdc_mem, self.old_obj);
                let _ = DeleteDC(self.hdc_mem);
            }
            if !self.hbmp.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.hbmp.0));
            }
            if !self.hicon.is_invalid() {
                let _ = DestroyIcon(self.hicon);
            }
            PostQuitMessage(0);
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        // One monotonic clock shared with the cursor sampler — accumulating
        // clamped dt here would drift trail aging on any tick slower than
        // 100ms.
        self.clock = self.t0.elapsed().as_secs_f32();

        let cfg = self.shared.settings.read().unwrap().clone();

        let want = ((1000.0 / cfg.render_fps.clamp(24, 240) as f32) as u32).max(8);
        if want != self.timer_period {
            self.timer_period = want;
            unsafe {
                SetTimer(self.hwnd, TIMER_ID, want, None);
            }
        }

        // Occasionally re-check monitor layout (DPI changes don't always send
        // WM_DISPLAYCHANGE to us).
        self.metric_checks += 1;
        if self.metric_checks >= 180 {
            self.metric_checks = 0;
            self.on_display_change();
        }

        // Windows sometimes demotes the overlay's z-position below ordinary
        // app windows while leaving WS_EX_TOPMOST set (observed after other
        // windows activate) — re-assert topmost periodically so the effects
        // stay on top. A no-op when already seated correctly.
        if self.last_topmost_assert.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_topmost_assert = now;
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }

        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let mx = (pt.x - self.vx) as f32;
        let my = (pt.y - self.vy) as f32;
        let prev = self.mouse;
        self.mouse = (mx, my);

        let dt_safe = dt.max(1e-4);
        let ivx = (mx - prev.0) / dt_safe;
        let ivy = (my - prev.1) / dt_safe;
        let mut vel = (self.vel.0 * 0.5 + ivx * 0.5, self.vel.1 * 0.5 + ivy * 0.5);
        let vmag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
        if vmag > 5000.0 {
            vel = (vel.0 / vmag * 5000.0, vel.1 / vmag * 5000.0);
        }
        self.vel = vel;

        let left = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 } & 0x8000 != 0;
        let clicked = left && !self.prev_left;
        self.prev_left = left;

        let moved = ((mx - prev.0).powi(2) + (my - prev.1).powi(2)).sqrt();

        // Feed the sampler's high-frequency cursor path into the trail. If the
        // sampler thread isn't available for some reason, fall back to a
        // single per-tick sample so the trail still appears.
        let mut samples: Vec<(f32, f32, f32)> = match &self.sampler {
            Some(s) => s
                .drain()
                .into_iter()
                .map(|(x, y, t)| (x as f32 - self.vx as f32, y as f32 - self.vy as f32, t))
                .collect(),
            None => Vec::new(),
        };
        if samples.is_empty() && moved > 0.5 {
            samples.push((mx, my, self.clock));
        }
        self.effects.push_trail_samples(samples);

        let ctx = effects::Ctx {
            dt,
            now: self.clock,
            mouse: self.mouse,
            vel,
            clicked,
            moving: moved,
            cfg: &cfg,
            screen: (self.w as f32, self.h as f32),
        };
        self.effects.update(&ctx);

        let cur = self.effects.bounds(&cfg);
        let dirty = effects::RectF::union(self.prev_bounds, cur);
        self.prev_bounds = cur;

        let dirty = match dirty {
            Some(d) => d,
            None => return, // nothing to draw and nothing to erase
        };

        let pad = 2.0;
        let x0 = ((dirty.x0 - pad).floor().max(0.0) as i32).max(0);
        let y0 = ((dirty.y0 - pad).floor().max(0.0) as i32).max(0);
        let x1 = (((dirty.x1 + pad).ceil() as i32).min(self.w)).max(x0);
        let y1 = (((dirty.y1 + pad).ceil() as i32).min(self.h)).max(y0);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        unsafe {
            let len = (self.w as usize) * (self.h as usize) * 4;
            let buf = std::slice::from_raw_parts_mut(self.bits, len);
            clear_rows(buf, self.w as usize, x0 as usize, x1 as usize, y0 as usize, y1 as usize);
            let mut pm = PixmapMut::from_bytes(buf, self.w as u32, self.h as u32)
                .expect("pixmap over DIB");
            self.effects.render(&mut pm, &cfg, self.clock);
            drop(pm);
        }

        self.update_layered(RECT { left: x0, top: y0, right: x1, bottom: y1 });

        if std::env::var("MOUSETRAILS_DUMP").is_ok() {
            self.dump_counter += 1;
            if self.dump_counter % 20 == 0 {
                let c = self.effects.counts();
                debug_log(format!(
                    "dump#{} counts(t,b,s,r)=({},{},{},{}) dirty=({},{})-({},{})",
                    self.dump_counter, c.0, c.1, c.2, c.3, x0, y0, x1, y1
                ));
                self.dump_frame();
            }
        }
    }

    fn dump_frame(&mut self) {
        unsafe {
            let len = (self.w as usize) * (self.h as usize) * 4;
            let mut data = vec![0u8; len];
            std::ptr::copy_nonoverlapping(self.bits, data.as_mut_ptr(), len);
            // Draw a marker circle into the COPY to verify the dump pipeline
            // itself (should always be visible at 300,300).
            if let Some(mut pm) =
                tiny_skia::Pixmap::from_vec(data.clone(), tiny_skia::IntSize::from_wh(self.w as u32, self.h as u32).unwrap())
            {
                let mut paint = tiny_skia::Paint::default();
                paint.anti_alias = true;
                paint.shader = tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(255, 0, 255, 255));
                let mut pb = tiny_skia::PathBuilder::new();
                pb.push_circle(300.0, 300.0, 60.0);
                if let Some(p) = pb.finish() {
                    pm.fill_path(&p, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
                }
            }
            // Stored as BGRA; flip back to RGBA for a viewable PNG.
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            if let Some(pm) =
                tiny_skia::Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(self.w as u32, self.h as u32).unwrap())
            {
                let path = std::env::temp_dir().join("mousetrails_dump.png");
                match pm.encode_png() {
                    Ok(bytes) => {
                        let _ = std::fs::write(&path, bytes);
                    }
                    Err(e) => debug_log(format!("dump encode err: {e}")),
                }
            }
        }
    }

    fn update_layered(&mut self, _dirty: RECT) {
        unsafe {
            // Push the whole window every rendered frame via
            // UpdateLayeredWindowIndirect with explicit position+size.
            // (Dirty-rect updates via prcDirty silently no-op on some
            // Windows builds, so we don't rely on them.)
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x: self.vx, y: self.vy };
            let size = SIZE { cx: self.w, cy: self.h };
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: HDC::default(),
                pptDst: &dst,
                psize: &size,
                hdcSrc: self.hdc_mem,
                pptSrc: &src,
                crKey: COLORREF(0),
                pblend: &blend,
                dwFlags: ULW_ALPHA,
                prcDirty: std::ptr::null(),
            };
            if !UpdateLayeredWindowIndirect(self.hwnd, &info).as_bool() {
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    HDC::default(),
                    Some(&dst),
                    Some(&size),
                    self.hdc_mem,
                    Some(&src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        }
    }
}

fn clear_rows(buf: &mut [u8], w: usize, x0: usize, x1: usize, y0: usize, y1: usize) {
    for y in y0..y1 {
        let off = y * w * 4 + x0 * 4;
        let end = y * w * 4 + x1 * 4;
        buf[off..end].fill(0);
    }
}
