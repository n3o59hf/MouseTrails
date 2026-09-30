#![windows_subsystem = "windows"]

mod effects;
mod overlay;
mod settings;
mod updater;
mod settings_ui;
mod startup;

use mousetrails::icon;

use std::sync::{Arc, Mutex, RwLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, MB_ICONINFORMATION, MB_SETFOREGROUND,
    MB_TOPMOST,
};

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let want_settings = std::env::args().any(|a| a == "--settings");

    // Clean up the old binary left behind by the last self-update.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::remove_file(dir.join("MouseTrails.prev.exe"));
        }
    }

    // Single instance guard. When restarted by the self-updater, the old
    // process may hold the mutex for a moment while it exits — retry a few
    // seconds instead of declaring a conflict.
    let update_restart = std::env::args().any(|a| a == "--update-restart");
    let attempts = if update_restart { 24 } else { 1 };
    let mut already_running = true;
    let mut mutex: Option<windows::Win32::Foundation::HANDLE> = None;
    for _ in 0..attempts {
        // Release the previous attempt's handle before re-checking, so we
        // don't keep the mutex object alive ourselves.
        if let Some(h) = mutex.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        let attempt = unsafe { CreateMutexW(None, true, w!("MouseTrails.SingleInstance")) };
        already_running = match &attempt {
            // ERROR_ALREADY_EXISTS (183): a previous instance still holds the mutex.
            Ok(h) => {
                mutex = Some(*h);
                std::io::Error::last_os_error().raw_os_error() == Some(183)
            }
            Err(_) => true, // couldn't create the mutex at all — be conservative
        };
        if !already_running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    if already_running {
        unsafe {
            if want_settings {
                if let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("MouseTrails Settings")) {
                    let _ = SetForegroundWindow(hwnd);
                }
            } else {
                let _ = MessageBoxW(
                    HWND::default(),
                    w!("MouseTrails is already running.\n\nLook for its rainbow icon near the clock — click it to open Settings, right-click for more options."),
                    w!("MouseTrails"),
                    MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
        }
        return;
    }

    let shared = Arc::new(settings::SharedSettings {
        settings: RwLock::new(settings::Settings::load()),
        update: Mutex::new(updater::UpdateState::initial()),
        overlay_hwnd: std::sync::atomic::AtomicIsize::new(0),
    });

    // First launch (no config yet): every effect is on by default — open the
    // settings window right away so the user sees what they can play with.
    let first_run = !settings::config_exists();
    overlay::run(shared, want_settings || first_run);
}
