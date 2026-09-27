#![windows_subsystem = "windows"]

mod effects;
mod icon;
mod overlay;
mod settings;
mod settings_ui;
mod startup;

use std::sync::{Arc, RwLock};

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

    // Single instance guard.
    let mutex = unsafe { CreateMutexW(None, true, w!("MouseTrails.SingleInstance")) };
    let already_running = match &mutex {
        // ERROR_ALREADY_EXISTS (183): a previous instance still holds the mutex.
        Ok(_) => std::io::Error::last_os_error().raw_os_error() == Some(183),
        Err(_) => true, // couldn't create the mutex at all — be conservative
    };

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
    });

    overlay::run(shared, want_settings);
}
