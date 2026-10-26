//! Finding a bundle in the Trash, and putting it back.
//!
//! A file on the home filesystem goes to `$XDG_DATA_HOME/Trash`. A file on
//! another mount goes to `<mount>/.Trash/<uid>` when the sticky `.Trash`
//! directory exists (the image creates `/var/.Trash` for `/var/Applications`),
//! otherwise to `<mount>/.Trash-<uid>`. The `.trashinfo` `Path` is absolute in
//! the home Trash and relative to the mount in the others.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashed {
    pub file: PathBuf,
    pub info: PathBuf,
    pub deleted: String,
}

/// One Trash directory, and the mount its relative paths start from.
#[derive(Clone, Debug)]
struct TrashDir {
    dir: PathBuf,
    top: Option<PathBuf>,
}

pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `Path` and `DeletionDate` from a `.trashinfo`.
pub fn parse_info(text: &str) -> Option<(String, String)> {
    let key = crate::keyfile::KeyFile::parse(text);
    let group = key.group("Trash Info")?;
    Some((
        decode(group.get("Path")?),
        group.get("DeletionDate").unwrap_or("").to_owned(),
    ))
}

/// The mount point `path` is on.
pub fn mount_point(path: &Path) -> Option<PathBuf> {
    let mut at = path.canonicalize().ok()?;
    let dev = std::fs::metadata(&at).ok()?.dev();
    while let Some(parent) = at.parent() {
        if std::fs::metadata(parent).ok()?.dev() != dev {
            break;
        }
        at = parent.to_path_buf();
    }
    Some(at)
}

fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// The Trash directories a file inside `folder` can land in, for this
/// account, or for every account when `everyone` is set.
fn dirs_for(folder: &Path, everyone: bool) -> Vec<TrashDir> {
    let mut dirs = vec![TrashDir {
        dir: crate::paths::data_home().join("Trash"),
        top: None,
    }];
    let Some(top) = mount_point(folder) else {
        return dirs;
    };
    let home_top = mount_point(&crate::paths::home());
    if home_top.as_deref() == Some(top.as_path()) && !everyone {
        return dirs;
    }
    if everyone {
        dirs.clear();
        let shared = top.join(".Trash");
        for e in std::fs::read_dir(&shared).into_iter().flatten().flatten() {
            dirs.push(TrashDir {
                dir: e.path(),
                top: Some(top.clone()),
            });
        }
        for e in std::fs::read_dir(&top).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(".Trash-") {
                dirs.push(TrashDir {
                    dir: e.path(),
                    top: Some(top.clone()),
                });
            }
        }
    } else {
        for dir in [
            top.join(".Trash").join(uid().to_string()),
            top.join(format!(".Trash-{}", uid())),
        ] {
            dirs.push(TrashDir {
                dir,
                top: Some(top.clone()),
            });
        }
    }
    dirs
}

fn same_place(a: &Path, b: &Path) -> bool {
    match (crate::paths::canonical(a), crate::paths::canonical(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// Every trashed item that was directly inside `folder`, with the path it
/// came from.
pub fn from_folder(folder: &Path, everyone: bool) -> Vec<(Trashed, PathBuf)> {
    let Some(folder_real) = folder.canonicalize().ok() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for trash in dirs_for(folder, everyone) {
        for e in std::fs::read_dir(trash.dir.join("info"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let info = e.path();
            let Some((path, deleted)) = std::fs::read_to_string(&info)
                .ok()
                .and_then(|t| parse_info(&t))
            else {
                continue;
            };
            let path = PathBuf::from(path);
            let path = match (&trash.top, path.is_absolute()) {
                (_, true) => path,
                (Some(top), false) => top.join(path),
                (None, false) => continue,
            };
            if crate::paths::canonical_parent(&path).as_deref() != Some(folder_real.as_path()) {
                continue;
            }
            let Some(stem) = info.file_stem() else {
                continue;
            };
            let file = trash.dir.join("files").join(stem);
            if std::fs::symlink_metadata(&file).is_ok() {
                out.push((
                    Trashed {
                        file,
                        info,
                        deleted,
                    },
                    path,
                ));
            }
        }
    }
    out
}

/// Whether a bundle for application `id` from `folder` is in a Trash.
pub fn holds_app(folder: &Path, id: &str, everyone: bool) -> bool {
    from_folder(folder, everyone)
        .iter()
        .any(|(t, _)| crate::Entry::read(&t.file).is_some_and(|e| e.id == id))
}

fn search(original: &Path, everyone: bool) -> Option<Trashed> {
    let folder = original.parent()?;
    let mut best: Option<Trashed> = None;
    for trash in dirs_for(folder, everyone) {
        let infos = trash.dir.join("info");
        for e in std::fs::read_dir(&infos).into_iter().flatten().flatten() {
            let info = e.path();
            if info.extension().is_none_or(|x| x != "trashinfo") {
                continue;
            }
            let Some((path, deleted)) = std::fs::read_to_string(&info)
                .ok()
                .and_then(|t| parse_info(&t))
            else {
                continue;
            };
            let path = PathBuf::from(path);
            let path = match (&trash.top, path.is_absolute()) {
                (_, true) => path,
                (Some(top), false) => top.join(path),
                (None, false) => continue,
            };
            if !same_place(&path, original) {
                continue;
            }
            let Some(stem) = info.file_stem() else {
                continue;
            };
            let file = trash.dir.join("files").join(stem);
            if std::fs::symlink_metadata(&file).is_err() {
                continue;
            }
            if best.as_ref().is_none_or(|b| deleted > b.deleted) {
                best = Some(Trashed {
                    file,
                    info,
                    deleted,
                });
            }
        }
    }
    best
}

/// The newest copy of `original` in this account's Trash.
pub fn find(original: &Path) -> Option<Trashed> {
    search(original, false)
}

/// Whether `original` is in any account's Trash on its mount. The root
/// service uses this before recreating an entry someone is deciding about.
pub fn in_anyones_trash(original: &Path) -> bool {
    search(original, true).is_some() || find(original).is_some()
}

/// Moves a trashed bundle back. Fails if something new took its place.
pub fn restore(trashed: &Trashed, to: &Path) -> crate::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(format!("{} already exists.", to.display()));
    }
    std::fs::rename(&trashed.file, to).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&trashed.info);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(decode("/a%20b/Firefox.eapp"), "/a b/Firefox.eapp");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%E2%9C%93"), "\u{2713}");
    }

    #[test]
    fn info_file() {
        let text =
            "[Trash Info]\nPath=Applications/My%20App.eapp\nDeletionDate=2026-10-25T21:40:00\n";
        assert_eq!(
            parse_info(text),
            Some((
                "Applications/My App.eapp".into(),
                "2026-10-25T21:40:00".into()
            ))
        );
    }

    #[test]
    fn finds_and_restores_from_home_trash() {
        let root = std::env::temp_dir().join(format!("eapp-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let apps = root.join("Applications");
        let trash = root.join("data/Trash");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(trash.join("files/Krita.eapp")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        let original = apps.join("Krita.eapp");
        std::fs::write(
            trash.join("info/Krita.eapp.trashinfo"),
            format!(
                "[Trash Info]\nPath={}\nDeletionDate=2026-10-25T21:40:00\n",
                original.display()
            ),
        )
        .unwrap();
        // SAFETY: the tests in this crate do not read XDG_DATA_HOME
        // concurrently with this one.
        unsafe { std::env::set_var("XDG_DATA_HOME", root.join("data")) };
        let found = find(&original).expect("in the trash");
        assert_eq!(found.file, trash.join("files/Krita.eapp"));
        restore(&found, &original).unwrap();
        assert!(original.is_dir());
        assert!(!found.info.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
