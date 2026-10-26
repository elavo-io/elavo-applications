//! Whether an application has a running process, for every account on the
//! machine. A running application cannot be deleted.

use std::path::Path;

use crate::{Entry, Kind};

/// A Flatpak's processes run in a systemd scope named
/// `app-flatpak-<id>-<n>.scope`. `/proc/<pid>/cgroup` is readable for every
/// process, so another account's instance is found too.
pub fn cgroup_names_flatpak(cgroup: &str, id: &str) -> bool {
    let needle = format!("app-flatpak-{id}-");
    cgroup.contains(&needle)
}

fn pids() -> impl Iterator<Item = std::path::PathBuf> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .map(|e| e.path())
}

pub fn flatpak_running(id: &str) -> bool {
    pids().any(|p| {
        std::fs::read_to_string(p.join("cgroup")).is_ok_and(|c| cgroup_names_flatpak(&c, id))
    })
}

/// The AppImage runtime keeps the AppImage as its executable and its first
/// argument while the application runs, so the runtime process stands for
/// the running application.
///
/// The process's first argument as a real path. The run script starts the
/// AppImage by a relative name, and a path may go through `/home` or
/// `/Applications`, which are symlinks.
fn first_argument(pid: &Path) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let cmdline = std::fs::read(pid.join("cmdline")).ok()?;
    let first = cmdline
        .split(|b| *b == 0)
        .next()
        .filter(|f| !f.is_empty())?;
    let first = Path::new(std::ffi::OsStr::from_bytes(first));
    let absolute = if first.is_absolute() {
        first.to_path_buf()
    } else {
        std::fs::read_link(pid.join("cwd")).ok()?.join(first)
    };
    Some(std::fs::canonicalize(&absolute).unwrap_or(absolute))
}

/// `bundle` should be the real path; the check also accepts the path through
/// the `/Applications` symlink.
pub fn appimage_running(bundle: &Path, also: Option<&Path>) -> bool {
    pids().any(|p| {
        let first = first_argument(&p);
        let exe = std::fs::read_link(p.join("exe")).ok();
        let inside = |b: &Path| {
            first.as_deref().is_some_and(|f| f.starts_with(b))
                || exe.as_deref().is_some_and(|e| e.starts_with(b))
        };
        inside(bundle) || also.is_some_and(inside)
    })
}

/// True when `entry`, installed at `bundle`, has a running process.
pub fn is_running(entry: &Entry, bundle: &Path) -> bool {
    match entry.kind {
        Kind::Flatpak => flatpak_running(&entry.id),
        Kind::Appimage => {
            let real = crate::paths::canonical(bundle);
            appimage_running(real.as_deref().unwrap_or(bundle), Some(bundle))
        }
        Kind::System => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_scope() {
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.mozilla.firefox-4410.scope\n";
        assert!(cgroup_names_flatpak(cg, "org.mozilla.firefox"));
        assert!(!cgroup_names_flatpak(cg, "org.mozilla"));
    }

    #[test]
    fn this_process() {
        let me = Path::new("/proc/self");
        let exe = std::fs::read_link(me.join("exe")).unwrap();
        let first = first_argument(me).unwrap();
        assert!(first.is_absolute());
        assert!(!first.starts_with(exe.with_extension("missing")));
        assert!(appimage_running(exe.parent().unwrap(), None));
    }
}
