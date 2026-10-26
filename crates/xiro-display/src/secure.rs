//! Admin-side driver install that non-admin processes can't tamper with: files are staged in a
//! fresh admin-only folder and held locked; the settings folder is read-only for everyone else.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    ACL, GetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_SHARE_READ};
use windows::core::{BOOL, HSTRING};

use crate::trust;

use crate::error::{DisplayError, Result};
use crate::install::{install_driver, restart_driver, settings_with};
use crate::Size;
use crate::vdd::{settings_dir, settings_file, settings_match};

/// Leftover marker from earlier releases; deleted on uninstall.
const LEGACY_RELOAD_MARKER: &str = "vdd_settings.xml.xiro-reload";

/// Only SYSTEM and Administrators, nothing inherited: for staging driver files.
const ADMIN_ONLY: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// Admins write; everyone reads (the driver host runs as a service account).
const ADMIN_WRITE_EVERYONE_READ: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;WD)";

/// A security descriptor parsed from SDDL, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn parse(sddl: &str) -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sd` receives a LocalAlloc'd descriptor that Drop frees.
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| DisplayError::new(format!("Could not build a security descriptor: {}", e.message())))?;
        Ok(Self(sd))
    }

    fn dacl(&self) -> Result<*const ACL> {
        let (mut present, mut defaulted) = (BOOL::default(), BOOL::default());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `self.0` is a valid descriptor; the DACL pointer points into it.
        unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            .map_err(|e| DisplayError::new(format!("Could not read a security descriptor: {}", e.message())))?;
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

/// Creates `dir` (which must not exist yet) with the given access rules.
fn create_dir_with(dir: &Path, sddl: &str) -> Result<()> {
    let descriptor = Descriptor::parse(sddl)?;
    let attributes = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0.0, bInheritHandle: false.into() };
    // SAFETY: `attributes` and the descriptor outlive the call.
    unsafe { CreateDirectoryW(&HSTRING::from(dir.as_os_str()), Some(&attributes)) }
        .map_err(|e| DisplayError::new(format!("Could not create {}: {}", dir.display(), e.message())))
}

fn random_suffix() -> String {
    // Unpredictable enough that nobody can prepare the folder in advance; it is created exclusively anyway.
    use std::hash::{BuildHasher, RandomState};
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    // RandomState is seeded from the system's random source.
    format!("{:x}{:x}{:x}", nanos, std::process::id(), RandomState::new().hash_one(nanos))
}

/// ADMIN: installs the embedded driver package. Files stay open (read-share only) in a new
/// admin-only folder until Windows has copied them, so they can't be swapped.
pub fn install_embedded_driver(files: &[(&str, &[u8])], inf_name: &str) -> Result<()> {
    let windows = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let dir = windows.join("Temp").join(format!("xiro-driver-{}", random_suffix()));
    create_dir_with(&dir, ADMIN_ONLY)?;
    let result = (|| {
        let mut held: Vec<File> = Vec::with_capacity(files.len());
        for (name, bytes) in files {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ.0)
                .open(dir.join(name))
                .map_err(|e| DisplayError::new(format!("Could not stage {name}: {e}")))?;
            file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| DisplayError::new(format!("Could not stage {name}: {e}")))?;
            held.push(file);
        }
        let installed = install_driver(&dir.join(inf_name));
        drop(held);
        installed
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

/// ADMIN: makes the driver offer exactly these sizes, restarting its device on change, and
/// locks the settings to admins. Returns whether the settings changed.
pub fn register_all(sizes: &[Size]) -> Result<bool> {
    let dir = settings_dir();
    let file = settings_file(&dir);
    prepare_settings_folder(&dir)?;
    // Only a plain file is read: never one with a second name somewhere else.
    let current = trust::is_plain_file(&file).then(|| fs::read_to_string(&file).ok()).flatten();
    // Rewritten whole: too many modes or an unsupported size stops the driver creating its monitor.
    if current.as_deref().is_some_and(|xml| settings_match(xml, sizes)) {
        return Ok(false);
    }
    trust::replace_file(&file, settings_with(sizes).as_bytes())?;
    // The driver reads settings when its device starts. Never send RELOAD_DRIVER: it crashes the driver.
    restart_driver()?;
    Ok(true)
}

/// ADMIN: a folder someone else created (or a link) is replaced by a fresh admin-only one;
/// Xiro's own only gets its permissions reset.
fn prepare_settings_folder(dir: &Path) -> Result<()> {
    let fresh = || {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).map_err(|e| DisplayError::new(format!("Could not create {}: {e}", parent.display())))?;
        }
        create_dir_with(dir, ADMIN_WRITE_EVERYONE_READ)
    };
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fresh(),
        Err(e) => Err(DisplayError::new(format!("Could not check {}: {e}", dir.display()))),
        Ok(meta) if trust::is_link(&meta) || !meta.is_dir() || !trust::owned_by_admins(dir) => {
            trust::remove_entry(dir, &meta)?;
            fresh()
        }
        Ok(_) => {
            let descriptor = Descriptor::parse(ADMIN_WRITE_EVERYONE_READ)?;
            trust::protect_folder(dir, descriptor.dacl()?)
        }
    }
}

/// Whether the admin step must run again; needs no admin.
pub fn settings_need_update(sizes: &[Size]) -> bool {
    fs::read_to_string(settings_file(&settings_dir())).map_or(true, |xml| !settings_match(&xml, sizes))
}

/// ADMIN, on uninstall: deletes the settings and legacy marker, then the folder only if empty.
pub fn remove_settings() -> Result<()> {
    let dir = settings_dir();
    let file = settings_file(&dir);
    for path in [dir.join(LEGACY_RELOAD_MARKER), file] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(DisplayError::new(format!("Could not delete {}: {e}", path.display()))),
        }
    }
    let _ = fs::remove_dir(&dir); // only succeeds when empty
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_rules_parse_and_staging_names_differ() {
        assert!(Descriptor::parse(ADMIN_ONLY).and_then(|d| d.dacl()).is_ok_and(|dacl| !dacl.is_null()));
        assert!(Descriptor::parse(ADMIN_WRITE_EVERYONE_READ).is_ok());
        assert!(Descriptor::parse("not sddl").is_err());
        let a = random_suffix();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(a, random_suffix());
    }

    #[test]
    fn protected_folders_are_created_exclusively() {
        // Owner-only rules for the current user: exercises the same calls without admin rights.
        let dir = std::env::temp_dir().join(format!("xiro-secure-{}", random_suffix()));
        create_dir_with(&dir, "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;CO)").unwrap();
        fs::write(dir.join("x"), b"1").unwrap();
        assert!(create_dir_with(&dir, ADMIN_ONLY).is_err(), "never reuses an existing folder");
        fs::remove_dir_all(&dir).unwrap();
    }
}
