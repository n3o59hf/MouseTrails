use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HMODULE};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::*;

const RUN_SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("MouseTrails");

pub fn is_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_SUBKEY, VALUE_NAME, RRF_RT_REG_SZ, None, None, None).ok().is_ok() }
}

fn exe_path() -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(HMODULE::default(), &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(1024)])
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, RUN_SUBKEY, 0, KEY_SET_VALUE, &mut hkey)
            .ok()
            .map_err(|e| e.to_string())?;

        let res = if on {
            let mut wide: Vec<u16> = format!("\"{}\"", exe_path()).encode_utf16().collect();
            wide.push(0);
            let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
            RegSetValueExW(hkey, VALUE_NAME, 0, REG_SZ, Some(&bytes))
        } else {
            // Deleting a value that isn't there is fine — the goal state is reached.
            match RegDeleteValueW(hkey, VALUE_NAME) {
                ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                other => other,
            }
        };

        let _ = RegCloseKey(hkey);
        res.ok()
            .map_err(|e: windows::core::Error| e.to_string())?;

        if is_enabled() == on {
            Ok(())
        } else {
            Err("Could not verify the registry change.".into())
        }
    }
}
