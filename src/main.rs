#![windows_subsystem = "windows"]

mod effects;
mod expr;
mod overlay;
mod settings;
mod store;
mod preview;
mod themes;
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
    FindWindowW, MessageBoxW, SetForegroundWindow, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_SETFOREGROUND, MB_TOPMOST,
};

/// Outcome of taking the single-instance named mutex.
enum InstanceGuard {
    /// We hold the mutex; this is the only instance.
    Acquired,
    /// Another instance holds the mutex and has not exited yet.
    Busy,
    /// The mutex could not be created at all (the OS error is carried for
    /// the user-facing message). Not evidence of another instance.
    Unavailable(windows::core::Error),
}

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
    let mut guard = InstanceGuard::Busy;
    let mut mutex: Option<windows::Win32::Foundation::HANDLE> = None;
    for attempt in 0..attempts {
        // Release the previous attempt's handle before re-checking, so we
        // don't keep the mutex object alive ourselves.
        if let Some(h) = mutex.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        guard = match unsafe { CreateMutexW(None, true, w!("MouseTrails.SingleInstance")) } {
            // ERROR_ALREADY_EXISTS (183): a previous instance still holds the
            // mutex. On any other success we own it: keep the handle for the
            // rest of the process so the name stays reserved.
            Ok(h) => {
                mutex = Some(h);
                if std::io::Error::last_os_error().raw_os_error() == Some(183) {
                    InstanceGuard::Busy
                } else {
                    InstanceGuard::Acquired
                }
            }
            // The mutex itself couldn't be created — that says nothing about
            // another instance, so guessing would be worse than running. Two
            // trays is recoverable; silently refusing to start is not.
            Err(e) => InstanceGuard::Unavailable(e),
        };
        if !matches!(guard, InstanceGuard::Busy) || attempt + 1 == attempts {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    match guard {
        InstanceGuard::Acquired => {}
        InstanceGuard::Busy => {
            // Hand off to the running instance. `--settings` must still be
            // able to say something when it can't find that instance's
            // window: FindWindowW matches any window with this title, so a
            // miss means the instance is still starting (or the settings
            // window is gone), not that we should exit silently.
            let focused = if want_settings {
                unsafe {
                    FindWindowW(PCWSTR::null(), w!("MouseTrails Settings"))
                        .map(|hwnd| SetForegroundWindow(hwnd).as_bool())
                        .unwrap_or(false)
                }
            } else {
                false
            };
            if !focused {
                let text = if want_settings {
                    w!("MouseTrails is already running.\n\nIts settings window could not be brought to the front \u{2014} look for the rainbow icon near the clock and click it to open Settings.")
                } else {
                    w!("MouseTrails is already running.\n\nLook for its rainbow icon near the clock — click it to open Settings, right-click for more options.")
                };
                unsafe {
                    let _ = MessageBoxW(
                        HWND::default(),
                        text,
                        w!("MouseTrails"),
                        MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
                    );
                }
            }
            return;
        }
        InstanceGuard::Unavailable(e) => {
            // Starting anyway risks a duplicate tray icon, which the user can
            // see and close. Say so rather than leaving them guessing why
            // there are two.
            unsafe {
                let text = format!(
                    "MouseTrails could not check whether it is already running:\n{e}\n\nStarting anyway — if you see two icons near the clock, right-click one and choose Exit."
                );
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = MessageBoxW(
                    HWND::default(),
                    PCWSTR(wide.as_ptr()),
                    w!("MouseTrails"),
                    MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
            // Not running under a mutex: skips the --update-restart retry
            // semantics below without pretending the guard succeeded.
        }
    }

    // Keep the mutex alive for the whole process: dropping the handle would
    // release the name and let a second instance start.
    let _mutex = mutex;

    let store = store::Store::from_default();

    // First launch (no config yet): every effect is on by default — open the
    // settings window right away so the user sees what they can play with.
    // Checked before load_app(), which puts a fresh config on disk.
    let first_run = !store::config_exists();

    let mut app_cfg = store.load_app();

    // Presets and profiles can be deleted behind our back (Explorer, another
    // editor); a selection pointing at one of them would otherwise linger in
    // config.json and resume the stale name if the id came back.
    if store.prune_app_profiles(&mut app_cfg) {
        store.save_app(&app_cfg);
    }

    // The active profile may not exist yet (fresh install, or a theme whose
    // profile hasn't been materialized): build it from the theme definition
    // (themed sections over factory defaults) and write it out so the file
    // is editable from the start.
    let profile = app_cfg
        .profiles
        .get(&app_cfg.theme)
        .cloned()
        .unwrap_or_else(|| store::DEFAULT_PROFILE.to_string());
    let effects = store.load_profile_for_use(&app_cfg.theme, &profile).unwrap_or_else(|| {
        // Missing (first use) or unreadable — the latter is backed up as
        // `.corrupt` by load_profile_for_use before this rewrite.
        let d = themes::materialize_profile(&store, &app_cfg.theme);
        store.save_profile(&app_cfg.theme, &profile, &d);
        d
    });

    let shared = Arc::new(store::SharedSettings {
        store,
        effects: RwLock::new(effects),
        app: RwLock::new(app_cfg),
        update: Mutex::new(updater::UpdateState::initial()),
        overlay_hwnd: std::sync::atomic::AtomicIsize::new(0),
    });

    overlay::run(shared, want_settings || first_run);
}
