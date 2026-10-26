//! Starts Xiro silently at Windows sign-in via the per-user Run key (no admin needed).

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW, RRF_RT_REG_SZ,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE_NAME: PCWSTR = w!("Xiro");

/// The command line Windows runs at sign-in.
pub fn command(exe: &std::path::Path) -> String {
    format!("\"{}\" --background", exe.display())
}

fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY, String> {
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) }.ok().map_err(|e| e.message())?;
    Ok(key)
}

pub fn enable(command: &str) -> Result<(), String> {
    let key = open(KEY_SET_VALUE)?;
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    let result = unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)) }.ok().map_err(|e| e.message());
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

/// Whether an entry was removed.
pub fn disable() -> bool {
    let Ok(key) = open(KEY_SET_VALUE) else { return false };
    let removed = unsafe { RegDeleteValueW(key, VALUE_NAME) }.is_ok();
    unsafe {
        let _ = RegCloseKey(key);
    }
    removed
}

/// The registered command, if any.
pub fn entry() -> Option<String> {
    let mut size = 0u32;
    unsafe {
        let probe = RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, RRF_RT_REG_SZ, None, None, Some(&mut size));
        if probe == ERROR_FILE_NOT_FOUND || probe.is_err() || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2];
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)).ok().ok()?;
        let text = String::from_utf16_lossy(&buf);
        Some(text.trim_end_matches('\0').to_owned())
    }
}

/// Whether autostart points at this executable.
pub fn is_enabled_for(exe: &std::path::Path) -> bool {
    entry().is_some_and(|e| e.eq_ignore_ascii_case(&command(exe)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_the_path_and_runs_in_the_background() {
        let exe = std::path::Path::new(r"C:\Users\me\AppData\Local\Programs\Xiro\Xiro.exe");
        assert_eq!(command(exe), r#""C:\Users\me\AppData\Local\Programs\Xiro\Xiro.exe" --background"#);
    }

    #[test]
    fn reading_the_entry_never_fails() {
        // Read-only: the user's real autostart setting is not changed by tests.
        let _ = entry();
        let _ = open(windows::Win32::System::Registry::KEY_QUERY_VALUE).is_ok();
    }
}
