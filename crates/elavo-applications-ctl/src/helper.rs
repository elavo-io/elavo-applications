//! `elavo-applications-helper`, run as root through `pkexec` when a standard
//! account installs into `/Applications`, which only administrators can
//! write. It copies one file in, owned by root; the root service installs
//! files root owns.
//!
//! ```text
//! elavo-applications-helper install-shared <file>
//! ```

use std::fs::File;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use elavo_applications_core::{appimage, file_name_safe, Folder};

fn allowed_name(file: &Path) -> bool {
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "appimage" | "flatpakref" | "flatpak")
}

/// Opens `file` with the caller's file-system identity, so root copies only
/// what the person who asked could read themselves.
fn open_as(file: &Path, uid: u32) -> Result<File, String> {
    // SAFETY: getpwuid returns a pointer into static storage or null; this
    // program is single-threaded.
    let gid = unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return Err(format!("no account with uid {uid}"));
        }
        (*pw).pw_gid
    };
    // SAFETY: setfsuid and setfsgid only change this thread's file-system
    // identity, and it is set back before anything else is opened.
    unsafe {
        libc::setfsgid(gid);
        libc::setfsuid(uid);
    }
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file);
    unsafe {
        libc::setfsuid(0);
        libc::setfsgid(0);
    }
    opened.map_err(|e| format!("{}: {e}", file.display()))
}

fn install_shared(file: &Path) -> Result<(), String> {
    let caller: u32 = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|u| u.parse().ok())
        .ok_or("run through pkexec")?;
    let mut source = open_as(file, caller)?;
    let meta = source.metadata().map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a file", file.display()));
    }
    let dir = Folder::Shared.dir();
    let name = file_name_safe(&file.file_name().ok_or("no file name")?.to_string_lossy());
    let tmp = dir.join(format!(".{name}.part"));
    let copied = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::io::copy(&mut source, &mut out).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| e.to_string())?;
        if !allowed_name(Path::new(&name)) && !appimage::is_appimage(&tmp) {
            return Err("Only AppImages and Flatpaks can be installed.".to_owned());
        }
        let mut target = dir.join(&name);
        let mut n = 2;
        while target.exists() {
            target = dir.join(format!("{n} {name}"));
            n += 1;
        }
        std::fs::rename(&tmp, &target).map_err(|e| e.to_string())
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied
}

fn main() -> ExitCode {
    // SAFETY: geteuid cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("elavo-applications-helper: run through pkexec");
        return ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["install-shared", file] => install_shared(Path::new(file)),
        _ => Err("usage: elavo-applications-helper install-shared <file>".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("elavo-applications-helper: {e}");
            ExitCode::FAILURE
        }
    }
}
