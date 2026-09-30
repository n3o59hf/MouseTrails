//! Self-update from the rolling "latest" GitHub release.
//!
//! The workflow on every push to `main` force-moves the `latest` tag to the
//! built commit and attaches `MouseTrails.exe` (+ a `.sha256`). This module
//! resolves that tag via the public GitHub API, compares it against the
//! build identity embedded by `build.rs`, downloads and verifies the new exe,
//! swaps it in with the rename trick, and restarts via a detached helper.

use sha2::{Digest, Sha256};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

pub const REPO_OWNER: &str = "n3o59hf";
pub const REPO_NAME: &str = "MouseTrails";

/// Debug log to %TEMP%\mousetrails_debug.log when MOUSETRAILS_DEBUG is set.
pub(crate) fn debug_log(line: &str) {
    if std::env::var("MOUSETRAILS_DEBUG").is_err() {
        return;
    }
    let path = std::env::temp_dir().join("mousetrails_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

pub fn build_sha() -> &'static str {
    env!("MOUSETRAILS_BUILD_SHA")
}

pub fn build_date() -> &'static str {
    env!("MOUSETRAILS_BUILD_DATE")
}

pub fn build_ts() -> i64 {
    // Parsed at compile time from the %ct seconds embedded by build.rs.
    const TS: &str = env!("MOUSETRAILS_BUILD_TS");
    TS.parse().unwrap_or(0)
}

/// Parses GitHub's fixed-format UTC timestamp ("2026-09-27T17:03:00Z") into
/// unix seconds. Days-from-civil algorithm; valid for the foreseeable future.
fn iso_to_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> {
        s.get(a..z)?.parse::<i64>().ok()
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // Howard Hinnant's days_from_civil.
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Shared, thread-safe status shown in the settings window and tray.
#[derive(Default)]
pub struct UpdateState {
    pub status: String,
    pub available_sha: Option<String>,
}

impl UpdateState {
    pub fn initial() -> Self {
        Self { status: "Not checked yet".into(), available_sha: None }
    }
}

pub struct RemoteBuild {
    pub sha: String,
    pub date: String,
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_os_error() -> String {
    std::io::Error::last_os_error().to_string()
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    to_wide(s)
}

struct HandleGuard(*mut core::ffi::c_void);
impl HandleGuard {
    fn get(&self) -> *mut core::ffi::c_void {
        self.0
    }
}
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// GET https://{host}{path} over TLS, following redirects. Returns the body.
fn http_get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    unsafe {
        let session = HandleGuard(
            WinHttpOpen(
                w!("MouseTrails"),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
        );
        if session.get().is_null() {
            return Err(format!("WinHttpOpen: {}", last_os_error()));
        }
        let host_w = to_wide(host);
        let connection = HandleGuard(
            WinHttpConnect(session.get(), PCWSTR(host_w.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0),
        );
        if connection.get().is_null() {
            return Err(format!("WinHttpConnect: {}", last_os_error()));
        }
        let path_w = to_wide(path);
        let request = HandleGuard(
            WinHttpOpenRequest(
                connection.get(),
                w!("GET"),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
        );
        if request.get().is_null() {
            return Err(format!("WinHttpOpenRequest: {}", last_os_error()));
        }
        WinHttpSendRequest(request.get(), None, None, 0, 0, 0)
            .map_err(|e| format!("WinHttpSendRequest: {e}"))?;
        WinHttpReceiveResponse(request.get(), std::ptr::null_mut())
            .map_err(|e| format!("WinHttpReceiveResponse: {e}"))?;

        let mut status: u32 = 0;
        let mut size = 4u32;
        let mut index: u32 = 0;
        WinHttpQueryHeaders(
            request.get(),
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut size,
            &mut index,
        )
        .map_err(|e| format!("WinHttpQueryHeaders: {e}"))?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }

        let mut body = Vec::new();
        loop {
            let mut available: u32 = 0;
            WinHttpQueryDataAvailable(request.get(), &mut available)
                .map_err(|e| format!("WinHttpQueryDataAvailable: {e}"))?;
            if available == 0 {
                break;
            }
            let chunk_start = body.len();
            body.resize(chunk_start + available as usize, 0);
            let mut read: u32 = 0;
            WinHttpReadData(
                request.get(),
                body[chunk_start..].as_mut_ptr() as *mut core::ffi::c_void,
                available,
                &mut read,
            )
            .map_err(|e| format!("WinHttpReadData: {e}"))?;
            body.truncate(chunk_start + read as usize);
            if read == 0 {
                break;
            }
        }
        Ok(body)
    }
}

/// Resolves the rolling `latest` tag to its commit (SHA + commit date).
pub fn check_remote() -> Result<RemoteBuild, String> {
    let ref_path = format!("/repos/{REPO_OWNER}/{REPO_NAME}/git/ref/tags/latest");
    let body = http_get("api.github.com", &ref_path)?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("bad tag JSON: {e}"))?;
    let full_sha = v["object"]["sha"]
        .as_str()
        .ok_or("tag response has no sha")?
        .to_string();
    let sha = full_sha.chars().take(7).collect::<String>();

    let commit_path = format!("/repos/{REPO_OWNER}/{REPO_NAME}/commits/{full_sha}");
    let body = http_get("api.github.com", &commit_path)?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("bad commit JSON: {e}"))?;
    let date = v["commit"]["committer"]["date"]
        .as_str()
        .ok_or("commit response has no date")?
        .to_string();
    Ok(RemoteBuild { sha, date })
}

/// A remote build counts as an update when its SHA differs from ours and its
/// commit time is strictly newer, compared as unix seconds (string dates
/// would misorder across timezones). Prevents "downgrading".
pub fn is_newer(remote: &RemoteBuild) -> bool {
    let mine = build_ts();
    if mine == 0 {
        return false; // local dev build without git info — never self-update
    }
    match iso_to_unix(&remote.date) {
        Some(remote_ts) => remote.sha != build_sha() && remote_ts > mine,
        None => false,
    }
}

/// Records a check result into the shared status. Returns whether an update
/// is available.
pub fn record_check(shared: &crate::settings::SharedSettings, remote: &RemoteBuild) -> bool {
    let available = is_newer(remote);
    let mut st = shared.update.lock().unwrap();
    if available {
        st.status = format!("Update available: build {} ({})", remote.sha, remote.date);
        st.available_sha = Some(remote.sha.clone());
    } else {
        st.status = format!("Up to date (build {})", build_sha());
        st.available_sha = None;
    }
    available
}

/// Downloads the newest release exe, verifies it, and stages it next to the
/// running exe. Returns the staged path.
pub fn download_and_stage() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe.parent().ok_or("exe has no parent dir")?;
    let staged = dir.join("MouseTrails.update");

    let bytes = http_get(
        "github.com",
        &format!("/{REPO_OWNER}/{REPO_NAME}/releases/latest/download/MouseTrails.exe"),
    )?;
    if bytes.len() < 100_000 || &bytes[..2] != b"MZ" {
        return Err("downloaded file is not a valid exe".into());
    }

    // Verify against the release checksum when the CI provides one.
    if let Ok(hash_body) = http_get(
        "github.com",
        &format!("/{REPO_OWNER}/{REPO_NAME}/releases/latest/download/MouseTrails.exe.sha256"),
    ) {
        let expected = String::from_utf8_lossy(&hash_body)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        if expected.len() == 64 {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let got = format!("{:x}", hasher.finalize());
            if got != expected {
                return Err(format!("checksum mismatch (expected {expected}, got {got})"));
            }
        }
    }

    debug_log(&format!("staged {} bytes", bytes.len()));
    std::fs::write(&staged, &bytes).map_err(|e| format!("staging write failed: {e}"))?;
    Ok(staged)
}

/// Swaps the staged update in (the running exe is renamed aside, which is
/// allowed while it runs) and arranges for the new exe to start in ~1s.
/// The caller must then exit the application.
pub fn apply_and_restart() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe.parent().ok_or("exe has no parent dir")?;
    let staged = dir.join("MouseTrails.update");
    let prev = dir.join("MouseTrails.prev.exe");
    if !staged.exists() {
        return Err("no staged update found".into());
    }

    let _ = std::fs::remove_file(&prev);
    std::fs::rename(&exe, &prev).map_err(|e| format!("rename current: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Roll back so the app stays functional.
        let _ = std::fs::rename(&prev, &exe);
        return Err(format!("rename staged: {e}"));
    }

    // Start the new exe directly with --update-restart: it waits for this
    // process to release the single-instance mutex. (A cmd-based delayed
    // helper proved fragile — quoting through the argument escaper made
    // `start` unreliable.)
    use std::os::windows::process::CommandExt;
    debug_log(&format!("spawning updated exe {exe:?}"));
    match std::process::Command::new(&exe)
        .arg("--update-restart")
        .creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn()
    {
        Ok(child) => {
            debug_log(&format!("updated exe pid={}", child.id()));
            Ok(())
        }
        Err(e) => {
            debug_log(&format!("updated exe spawn FAILED: {e}"));
            Err(format!("spawn updated exe: {e}"))
        }
    }
}

/// Spawns a quiet check thread (used by the settings window): records the
/// result into the shared status without any dialog.
pub fn spawn_check(shared: std::sync::Arc<crate::settings::SharedSettings>) {
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || match check_remote() {
            Ok(remote) => {
                record_check(&shared, &remote);
            }
            Err(e) => {
                shared.update.lock().unwrap().status = format!("Update check failed: {e}");
            }
        })
        .ok();
}

/// Spawns the install worker: download, verify, stage, swap, and ask the
/// overlay to exit — the detached helper starts the new exe ~1s later.
pub fn spawn_install(shared: std::sync::Arc<crate::settings::SharedSettings>) {
    std::thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            {
                let mut st = shared.update.lock().unwrap();
                st.status = "Downloading update…".into();
            }
            match download_and_stage() {
                Ok(_) => {
                    {
                        let mut st = shared.update.lock().unwrap();
                        st.status = "Restarting to apply the update…".into();
                    }
                    match apply_and_restart() {
                        Ok(()) => {
                            let hwnd = shared.overlay_hwnd.load(std::sync::atomic::Ordering::Relaxed);
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                    windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
                                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                                    windows::Win32::Foundation::WPARAM(0),
                                    windows::Win32::Foundation::LPARAM(0),
                                );
                            }
                        }
                        Err(e) => {
                            shared.update.lock().unwrap().status = format!("Update failed: {e}");
                        }
                    }
                }
                Err(e) => {
                    shared.update.lock().unwrap().status = format!("Update failed: {e}");
                }
            }
        })
        .ok();
}
