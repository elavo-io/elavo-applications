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

    // An entry is made once, when its Flatpak is first seen. A person who
    // deletes a shared entry is asked Uninstall or Keep by their own
    // service, so an entry written back here would beat that question.
    let seen = seen_flatpaks();
    let mirror = reconcile::mirror(
        folder,
        |id| trash::holds_app(&dir, id, true) || seen.as_ref().is_some_and(|s| s.contains(id)),
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
    if let Some(installed) = flatpak::list(folder.scope()) {
        let ids: Vec<&str> = installed.iter().map(|a| a.id.as_str()).collect();
        if let Err(e) = save_seen_flatpaks(&ids) {
            fail("state", e);
        }
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

fn seen_path() -> std::path::PathBuf {
    paths::system_state().join("seen-flatpaks")
}

/// The system Flatpaks the last pass saw. None before the first pass, so
/// that pass makes an entry for every installed Flatpak.
fn seen_flatpaks() -> Option<HashSet<String>> {
    let text = std::fs::read_to_string(seen_path()).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// An uninstalled Flatpak drops out, so installing it again makes an entry.
fn save_seen_flatpaks(ids: &[&str]) -> Result<(), String> {
    let mut text = ids.join("\n");
    text.push('\n');
    let path = seen_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    bundle::write_file(&path, text.as_bytes(), 0o644)
}

/// Root made this bundle; administrators must still be able to delete it.
/// Removing the directory needs write access to it and to the folder.
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
