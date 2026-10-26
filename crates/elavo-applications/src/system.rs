//! One pass of the root service for `/Applications`: locked entries for the
//! system applications the image lists, an entry for every system Flatpak,
//! installs of files root owns (copied in by the helper for a standard
//! account), and the shared desktop-file exports.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use elavo_applications_core::reconcile::{self, Drop};
use elavo_applications_core::{bundle, flatpak, paths, system, trash, Folder, Kind};

pub fn run() -> ExitCode {
    let folder = Folder::Shared;
    let dir = folder.dir();
    if !dir.is_dir() {
        eprintln!("elavo-applications: {} does not exist", dir.display());
        return ExitCode::FAILURE;
    }
    let mut failed = false;
    let mut fail = |what: &str, e: String| {
        eprintln!("elavo-applications: {what}: {e}");
        failed = true;
    };

    let wanted = system::allowlist();
    for id in &wanted {
        match bundle::create_system(&dir, id) {
            Ok(b) => lock(&b.path),
            Err(e) => fail("system entry", e),
        }
    }
    for b in bundle::list(&dir) {
        if b.entry.kind == Kind::System && !wanted.contains(&b.entry.id) {
            let _ = std::fs::remove_dir_all(&b.path);
        }
    }

    // SAFETY: getuid cannot fail.
    let me = unsafe { libc::getuid() };
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = e.path();
        if paths::is_bundle(&path) || reconcile::owner(&path) != Some(me) {
            continue;
        }
        if let Err(err) = install(folder, &path) {
            fail(&path.display().to_string(), err);
        }
    }

    let mirror = reconcile::mirror(
        folder,
        |id| trash::holds_app(&dir, id, true),
        &HashSet::new(),
    );
    match mirror {
        Ok(m) => {
            for p in m.created {
                open_to_admins(&p);
            }
        }
        Err(e) => fail("flatpak", e),
    }

    for b in bundle::list(&dir) {
        if b.entry.kind != Kind::System {
            let _ = bundle::refresh(&b);
        }
    }
    if let Err(e) = bundle::sync_exports(folder) {
        fail("exports", e);
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn install(folder: Folder, path: &Path) -> Result<(), String> {
    let scope = folder.scope();
    match reconcile::classify(path) {
        Drop::AppImage => {
            let inspected = bundle::inspect_appimage(path);
            let existing = bundle::find(&folder.dir(), Kind::Appimage, &inspected.id);
            let b = bundle::install_appimage(folder, path, &inspected, existing.as_ref())?;
            open_to_admins(&b.path);
            Ok(())
        }
        Drop::FlatpakRef => {
            flatpak::install_ref(path, scope)?;
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        Drop::FlatpakBundle => {
            flatpak::install_bundle(path, scope)?;
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        Drop::Other | Drop::Ignore => Ok(()),
    }
}

/// Root made this bundle; administrators must still be able to move it to
/// the Trash, which renames the directory and needs write access to it.
/// The setgid folder gives it the `wheel` group.
fn open_to_admins(path: &Path) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o2775));
}

/// Locked entries cannot be renamed by anyone but root. With the folder's
/// default ACL, the group bits are the ACL mask, so `0755` also takes write
/// access away from `wheel`.
fn lock(path: &Path) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}
