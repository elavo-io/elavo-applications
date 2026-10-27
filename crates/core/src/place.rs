//! Putting a file in an Applications folder, where a service installs it.

use std::path::Path;

use crate::paths::{self, Folder};

const HELPER: &str = "/usr/libexec/elavo-applications-helper";

/// Moves `file` into `folder`; the service installs it from there. A
/// standard account installing for everyone goes through the root helper,
/// which asks for an administrator's password and copies the file.
pub fn place(file: &Path, folder: Folder) -> crate::Result<()> {
    let file = file
        .canonicalize()
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let dir = folder.dir();
    if folder == Folder::Shared && !paths::writable(&dir) {
        let status = std::process::Command::new("pkexec")
            .arg(HELPER)
            .arg("install-shared")
            .arg(&file)
            .status()
            .map_err(|e| format!("pkexec: {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err("The administrator password was not accepted.".to_owned())
        };
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = file.file_name().ok_or("not a file")?;
    let target = dir.join(name);
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    if std::fs::rename(&file, &target).is_err() {
        std::fs::copy(&file, &target).map_err(|e| e.to_string())?;
    }
    Ok(())
}
