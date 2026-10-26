//! Per-user folders, asked from the shell so redirected profiles are honoured; env vars are a fallback.

use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_Programs, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::GUID;

pub(crate) const APP_NAME: &str = "Xiro";
const SHORTCUT_NAME: &str = "Xiro.lnk";

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: on success the shell returns a CoTaskMemAlloc'd string that we copy and free.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as *const _));
        path
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub(crate) fn local_app_data() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData)
        .or_else(|| env_dir("LOCALAPPDATA"))
        .or_else(|| env_dir("USERPROFILE").map(|home| home.join("AppData").join("Local")))
        .unwrap_or_else(std::env::temp_dir)
}

/// %LOCALAPPDATA%\Programs\Xiro: the per-user program folder Windows itself suggests.
pub fn install_dir() -> PathBuf {
    local_app_data().join("Programs").join(APP_NAME)
}

/// %LOCALAPPDATA%\Xiro: certificates, settings and logs (kept on uninstall unless asked).
pub(crate) fn data_dir() -> PathBuf {
    local_app_data().join(APP_NAME)
}

/// Start Menu\Programs\Xiro.lnk of the current user.
pub(crate) fn shortcut_path() -> Option<PathBuf> {
    known_folder(&FOLDERID_Programs)
        .or_else(|| env_dir("APPDATA").map(|d| d.join(r"Microsoft\Windows\Start Menu\Programs")))
        .map(|dir| dir.join(SHORTCUT_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_resolve_under_the_user_profile() {
        // Read-only lookups; nothing is created.
        let dir = install_dir();
        assert!(dir.ends_with(r"Programs\Xiro"), "{}", dir.display());
        assert!(dir.is_absolute());
        assert!(data_dir().ends_with("Xiro"));
        assert_ne!(data_dir(), dir);
        let shortcut = shortcut_path().expect("Start Menu folder");
        assert!(shortcut.ends_with(r"Start Menu\Programs\Xiro.lnk"), "{}", shortcut.display());
    }
}
