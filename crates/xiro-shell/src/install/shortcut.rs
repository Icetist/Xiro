//! The Start Menu shortcut created by legacy installs.

use std::path::Path;

pub(crate) fn remove(link: &Path) -> Result<(), String> {
    match std::fs::remove_file(link) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("could not remove {}: {e}", link.display())),
        _ => Ok(()),
    }
}
