//! Elevated setup writes to a folder at the root of C: that a user could pre-create with links,
//! so it only writes into admin-owned folders, never through a link, and replaces files.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

use crate::error::{DisplayError, Result};

const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
/// Needed to open a folder at all.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Opens a link itself, never what it points to.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

fn fail(what: &str, path: &Path, err: impl std::fmt::Display) -> DisplayError {
    DisplayError::new(format!("{what} {}: {err}", path.display()))
}

/// A symlink or junction (or any other reparse point).
pub(crate) fn is_link(meta: &Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Opens `path` itself (not a link's target) with `access`.
fn open_raw(path: &Path, access: u32) -> std::io::Result<File> {
    OpenOptions::new().access_mode(access).custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(path)
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

/// Whether Administrators or SYSTEM own the folder or file (not a normal user).
pub(crate) fn owned_by_admins(path: &Path) -> bool {
    let Ok(file) = open_raw(path, READ_CONTROL) else { return false };
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: out-parameters are valid; the descriptor (which `owner` points into) is freed below.
    let rc = unsafe {
        GetSecurityInfo(handle(&file), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut descriptor))
    };
    if rc.is_err() {
        return false;
    }
    // SAFETY: `owner` points into the live descriptor.
    let ok = unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() || IsWellKnownSid(owner, WinLocalSystemSid).as_bool() };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    ok
}

/// A plain file with exactly one name: not a link, and not a hard link to some other file.
pub(crate) fn is_plain_file(path: &Path) -> bool {
    let Ok(file) = open_raw(path, FILE_READ_ATTRIBUTES) else { return false };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is a valid out-parameter for this handle.
    let ok = unsafe { GetFileInformationByHandle(handle(&file), &mut info) }.is_ok();
    ok && info.nNumberOfLinks == 1 && info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) == 0
}

/// Sets only the folder's own DACL; SetNamedSecurityInfo would also rewrite every file inside
/// (and whatever a hard link in it points to).
pub(crate) fn protect_folder(dir: &Path, dacl: *const ACL) -> Result<()> {
    let file = open_raw(dir, READ_CONTROL | WRITE_DAC).map_err(|e| fail("Could not open", dir, e))?;
    let info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    // SAFETY: the handle is open with WRITE_DAC; the caller keeps the DACL alive.
    let rc = unsafe { SetSecurityInfo(handle(&file), SE_FILE_OBJECT, info, None, None, Some(dacl), None) };
    rc.ok().map_err(|e| fail("Could not protect", dir, e.message()))
}

/// Removes what is at `path` without following it: a link goes, its target stays.
pub(crate) fn remove_entry(path: &Path, meta: &Metadata) -> Result<()> {
    let removed = match (is_link(meta), meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0) {
        (true, true) => fs::remove_dir(path),
        (_, false) => fs::remove_file(path),
        // A real folder: its contents too (Rust's remove_dir_all never follows links inside).
        (false, true) => fs::remove_dir_all(path),
    };
    removed.map_err(|e| fail("Could not remove", path, e))
}

/// Writes a new file beside `file`, then renames it over, so nothing at either name is written through.
pub(crate) fn replace_file(file: &Path, contents: &[u8]) -> Result<()> {
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".xiro-tmp");
    let tmp = PathBuf::from(tmp);
    if let Ok(meta) = fs::symlink_metadata(&tmp) {
        remove_entry(&tmp, &meta)?;
    }
    let mut out = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| fail("Could not write", &tmp, e))?;
    out.write_all(contents).and_then(|()| out.sync_all()).map_err(|e| fail("Could not write", &tmp, e))?;
    drop(out);
    fs::rename(&tmp, file).map_err(|e| fail("Could not replace", file, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xiro-trust-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hard_links_are_not_plain_files() {
        let dir = temp("links");
        let a = dir.join("a.xml");
        fs::write(&a, b"x").unwrap();
        assert!(is_plain_file(&a));
        fs::hard_link(&a, dir.join("b.xml")).unwrap();
        assert!(!is_plain_file(&a), "a second name for the same file");
        assert!(!is_plain_file(&dir), "a folder");
        assert!(!is_plain_file(&dir.join("missing")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replacing_never_writes_through_an_existing_hard_link() {
        let dir = temp("replace");
        let victim = dir.join("victim.txt");
        fs::write(&victim, b"keep me").unwrap();
        let settings = dir.join("settings.xml");
        fs::hard_link(&victim, &settings).unwrap();
        replace_file(&settings, b"<new/>").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"keep me");
        assert_eq!(fs::read(&settings).unwrap(), b"<new/>");
        assert!(is_plain_file(&settings));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_users_own_folder_is_not_trusted() {
        // Elevated (as on CI), new folders are owned by Administrators, so this only holds unelevated.
        let elevated = std::process::Command::new("net").arg("session").output().is_ok_and(|o| o.status.success());
        let dir = temp("owner");
        assert_eq!(owned_by_admins(&dir), elevated);
        fs::remove_dir_all(&dir).unwrap();
    }
}
