//! The work both services share: what a new file in a folder is, and keeping
//! Flatpak entries in step with what is installed.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{appimage, bundle, flatpak, Folder, Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    AppImage,
    FlatpakRef,
    FlatpakBundle,
    /// Something Elavo does not install.
    Other,
    /// Not a drop: hidden files, partial downloads, our own temporary files.
    Ignore,
}

pub fn classify(path: &Path) -> Drop {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    if name.starts_with('.')
        || ["~", ".part", ".crdownload", ".download", ".tmp"]
            .iter()
            .any(|s| lower.ends_with(s))
    {
        return Drop::Ignore;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Drop::Ignore;
    };
    if !meta.file_type().is_file() {
        return if meta.is_dir() && !crate::paths::is_bundle(path) {
            Drop::Other
        } else {
            Drop::Ignore
        };
    }
    if lower.ends_with(".flatpakref") {
        Drop::FlatpakRef
    } else if lower.ends_with(".flatpak") {
        Drop::FlatpakBundle
    } else if appimage::has_appimage_name(path) || appimage::is_appimage(path) {
        Drop::AppImage
    } else {
        Drop::Other
    }
}

pub fn owner(path: &Path) -> Option<u32> {
    std::fs::symlink_metadata(path).ok().map(|m| m.uid())
}

/// True when the directory was created or renamed within `secs`. A bundle
/// that just came back from the Trash is not "an entry whose app is gone".
pub fn changed_within(path: &Path, secs: u64) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let ctime = SystemTime::UNIX_EPOCH + Duration::from_secs(meta.ctime().max(0) as u64);
    SystemTime::now()
        .duration_since(ctime)
        .is_ok_and(|age| age.as_secs() < secs)
}

#[derive(Debug, Default)]
pub struct Mirror {
    pub created: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Every Flatpak installed in the folder's scope gets an entry, unless its
/// entry is in a Trash waiting for a decision. An entry whose Flatpak is no
/// longer installed is removed, unless it just appeared (it came back from
/// the Trash and is being reinstalled) or `busy` names it.
pub fn mirror(
    folder: Folder,
    in_trash: impl Fn(&str) -> bool,
    busy: &HashSet<String>,
) -> crate::Result<Mirror> {
    let scope = folder.scope();
    let dir = folder.dir();
    let installed = flatpak::list(scope).ok_or("flatpak could not list applications")?;
    let bundles = bundle::list(&dir);
    let mut out = Mirror::default();
    let ids: HashSet<&str> = installed.iter().map(|a| a.id.as_str()).collect();
    for b in &bundles {
        if b.entry.kind != Kind::Flatpak || b.entry.scope != scope {
            continue;
        }
        if ids.contains(b.entry.id.as_str()) || busy.contains(&b.entry.id) {
            continue;
        }
        if changed_within(&b.path, 120) {
            continue;
        }
        if std::fs::remove_dir_all(&b.path).is_ok() {
            out.removed.push(b.path.clone());
        }
    }
    for app in &installed {
        if busy.contains(&app.id) {
            continue;
        }
        if bundles
            .iter()
            .any(|b| b.entry.kind == Kind::Flatpak && b.entry.id == app.id)
        {
            continue;
        }
        if in_trash(&app.id) {
            continue;
        }
        out.created
            .push(bundle::create_flatpak(&dir, app, scope)?.path);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops() {
        let dir = std::env::temp_dir().join(format!("eapp-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Folder")).unwrap();
        std::fs::create_dir_all(dir.join("Krita.eapp")).unwrap();
        for name in [
            "a.flatpakref",
            "b.flatpak",
            "c.AppImage",
            "d.txt",
            ".hidden",
            "e.AppImage.part",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(classify(&dir.join("a.flatpakref")), Drop::FlatpakRef);
        assert_eq!(classify(&dir.join("b.flatpak")), Drop::FlatpakBundle);
        assert_eq!(classify(&dir.join("c.AppImage")), Drop::AppImage);
        assert_eq!(classify(&dir.join("d.txt")), Drop::Other);
        assert_eq!(classify(&dir.join(".hidden")), Drop::Ignore);
        assert_eq!(classify(&dir.join("e.AppImage.part")), Drop::Ignore);
        assert_eq!(classify(&dir.join("Folder")), Drop::Other);
        assert_eq!(classify(&dir.join("Krita.eapp")), Drop::Ignore);
        assert!(changed_within(&dir.join("Folder"), 60));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
