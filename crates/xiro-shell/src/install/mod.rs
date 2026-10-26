//! System changes for the portable exe: the firewall rule (via an elevated child process),
//! clean-up of legacy installs, and fallback message boxes.

mod cmdline;
mod com;
mod dialogs;
mod elevation;
mod firewall;
mod paths;
mod shortcut;
mod uninstall_entry;

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use paths::{data_dir, shortcut_path};

/// Keeps the delayed clean-up from flashing a console.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// %LOCALAPPDATA%\Programs\Xiro, the legacy install folder.
pub fn legacy_dir() -> PathBuf {
    paths::install_dir()
}

/// Removes legacy install leftovers and, with `remove_data`, settings, pairing and logs.
/// Folders go after this process exits (its log is still open); all errors are reported.
pub fn remove_leftovers(remove_data: bool) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Some(link) = shortcut_path() {
        errors.extend(shortcut::remove(&link).err());
    }
    errors.extend(uninstall_entry::remove().err());
    let legacy = legacy_dir();
    let data = data_dir();
    let mut folders: Vec<&Path> = vec![legacy.as_path()];
    if remove_data {
        folders.push(data.as_path());
    }
    errors.extend(schedule_removal(&folders).err());
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// Guards `rmdir /s` against an unexpected profile lookup result.
fn is_our_folder(folder: &Path) -> bool {
    folder.is_absolute() && folder.file_name().is_some_and(|name| name.eq_ignore_ascii_case(paths::APP_NAME))
}

fn schedule_removal(folders: &[&Path]) -> Result<(), String> {
    let running = std::env::current_exe().ok();
    let holds_us = |f: &Path| running.as_deref().is_some_and(|exe| exe.starts_with(f));
    let existing: Vec<&Path> = folders.iter().copied().filter(|f| is_our_folder(f) && f.exists() && !holds_us(f)).collect();
    if existing.is_empty() {
        return Ok(());
    }
    let arguments = cmdline::cleanup_command(&existing)?;
    let system_root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    Command::new(system_root.join(r"System32\cmd.exe"))
        .raw_arg(arguments)
        // System32: cmd would lock the folder being deleted, and it looks for `ping` in its cwd first.
        .current_dir(system_root.join("System32"))
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not schedule removal of Xiro's folders: {e}"))
}

/// Runs this exe elevated (UAC) and returns its exit code; Err if the prompt was declined.
pub fn run_elevated(args: &[&str]) -> Result<u32, String> {
    elevation::run_elevated(args)
}

pub fn is_elevated() -> bool {
    elevation::is_elevated()
}

/// ADMIN: replaces the inbound TCP rule "Xiro" for `exe` on `ports` (all profiles, LocalSubnet).
pub fn add_firewall_rule(exe: &Path, ports: &[u16]) -> Result<(), String> {
    firewall::add(exe, ports)
}

/// ADMIN: removes the "Xiro" rule(s).
pub fn remove_firewall_rule() -> Result<(), String> {
    firewall::remove()
}

/// Whether the "Xiro" rule allows exactly this exe and ports; `None` if unreadable (don't ask then).
pub fn firewall_rule_matches(exe: &Path, ports: &[u16]) -> Option<bool> {
    let set = |list: &str| {
        let mut ports: Vec<String> = list.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();
        ports.sort();
        ports
    };
    let wanted = set(&firewall::port_list(ports));
    firewall::program().map(|rule| {
        rule.is_some_and(|(program, open)| program.eq_ignore_ascii_case(&exe.display().to_string()) && set(&open) == wanted)
    })
}

/// A modal Yes/No question.
pub fn ask(title: &str, text: &str) -> bool {
    dialogs::ask(title, text)
}

/// Like `ask`, but No is the default so a reflexive Enter doesn't say yes.
pub fn confirm(title: &str, text: &str) -> bool {
    dialogs::confirm(title, text)
}

pub fn message(title: &str, text: &str, is_error: bool) {
    dialogs::message(title, text, is_error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_xiro_folders_are_ever_scheduled_for_removal() {
        assert!(is_our_folder(Path::new(r"C:\Users\me\AppData\Local\Programs\Xiro")));
        assert!(is_our_folder(Path::new(r"C:\Users\me\AppData\Local\xiro")));
        assert!(!is_our_folder(Path::new(r"C:\Users\me\AppData\Local")));
        assert!(!is_our_folder(Path::new(r"Xiro")));
        assert!(!is_our_folder(Path::new(r"C:\")));
        assert!(is_our_folder(&legacy_dir()));
        assert!(is_our_folder(&data_dir()));
    }
}
