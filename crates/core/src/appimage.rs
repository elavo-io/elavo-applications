//! Reading an AppImage without running it. A type 2 AppImage is an ELF
//! runtime followed by a squashfs image; the image starts where the ELF's
//! section header table ends. `unsquashfs -o` reads the desktop file and the
//! icon from there. `--appimage-extract` would run code the person has not
//! chosen to run yet.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::keyfile::{Group, KeyFile};

#[derive(Clone, Debug, Default)]
pub struct Info {
    /// The embedded desktop file's id, without `.desktop`.
    pub desktop_stem: Option<String>,
    /// Its `[Desktop Entry]` and action groups.
    pub desktop: Option<KeyFile>,
    /// The bundled icon, copied out to a temporary file.
    pub icon: Option<PathBuf>,
}

impl Info {
    pub fn entry(&self) -> Option<&Group> {
        self.desktop.as_ref().and_then(|d| d.entry())
    }

    pub fn name(&self) -> Option<String> {
        self.entry()
            .and_then(|e| e.get("Name"))
            .map(str::to_owned)
            .filter(|n| !n.is_empty())
    }
}

pub fn has_appimage_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
}

/// The magic bytes AppImages carry at offset 8: `AI` and the type.
pub fn appimage_type(header: &[u8]) -> Option<u8> {
    if header.len() < 11 || &header[0..4] != b"\x7fELF" || &header[8..10] != b"AI" {
        return None;
    }
    Some(header[10])
}

/// Where the squashfs image starts: the end of the ELF section headers.
pub fn squashfs_offset(header: &[u8]) -> Option<u64> {
    if header.len() < 64 || &header[0..4] != b"\x7fELF" {
        return None;
    }
    let little = header[5] == 1;
    let u16_at = |at: usize| {
        let b = [header[at], header[at + 1]];
        if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    };
    let u32_at = |at: usize| {
        let b: [u8; 4] = header[at..at + 4].try_into().unwrap();
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    let u64_at = |at: usize| {
        let b: [u8; 8] = header[at..at + 8].try_into().unwrap();
        if little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        }
    };
    let (shoff, shentsize, shnum) = match header[4] {
        2 => (u64_at(0x28), u16_at(0x3a), u16_at(0x3c)),
        1 => (u64::from(u32_at(0x20)), u16_at(0x2e), u16_at(0x30)),
        _ => return None,
    };
    Some(shoff + u64::from(shentsize) * u64::from(shnum))
}

fn header(path: &Path) -> Option<(Vec<u8>, u64)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64];
    file.read_exact(&mut buf).ok()?;
    let len = file.seek(SeekFrom::End(0)).ok()?;
    Some((buf, len))
}

pub fn is_appimage(path: &Path) -> bool {
    header(path).is_some_and(|(h, _)| appimage_type(&h).is_some())
}

fn unsquash(file: &Path, offset: u64, dest: &Path, items: &[&str]) -> bool {
    Command::new("unsquashfs")
        .arg("-o")
        .arg(offset.to_string())
        .args(["-q", "-n", "-f", "-no-xattrs", "-d"])
        .arg(dest)
        .arg(file)
        .args(items)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Follows a symlink inside the extracted tree, extracting each target.
fn resolve_in_image(file: &Path, offset: u64, dest: &Path, mut rel: PathBuf) -> Option<PathBuf> {
    for _ in 0..8 {
        let at = dest.join(&rel);
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            return None;
        };
        if !meta.file_type().is_symlink() {
            return meta.is_file().then_some(at);
        }
        let target = std::fs::read_link(&at).ok()?;
        let next = if target.is_absolute() {
            target.strip_prefix("/").ok()?.to_path_buf()
        } else {
            rel.parent().unwrap_or(Path::new("")).join(target)
        };
        let next = normalize(&next)?;
        let item = next.to_str()?.to_owned();
        let _ = std::fs::remove_file(&at);
        if !unsquash(file, offset, dest, &[&item]) {
            return None;
        }
        rel = next;
    }
    None
}

/// Removes `.` and `..` without touching the filesystem. None when it would
/// climb out of the image.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::Normal(p) => out.push(p),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Reads the desktop file and icon. `scratch` is an empty directory the
/// caller removes afterwards. A file that cannot be read still gets an
/// entry; it just has no name or icon from inside.
pub fn read(file: &Path, scratch: &Path) -> Info {
    let mut info = Info::default();
    let Some((head, len)) = header(file) else {
        return info;
    };
    if appimage_type(&head) != Some(2) {
        return info;
    }
    let Some(offset) = squashfs_offset(&head).filter(|o| *o < len) else {
        return info;
    };
    if !unsquash(file, offset, scratch, &["*.desktop", ".DirIcon"]) {
        return info;
    }
    let desktop = std::fs::read_dir(scratch)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
        .find_map(|p| {
            let rel = PathBuf::from(p.file_name()?);
            resolve_in_image(file, offset, scratch, rel)
        });
    if let Some(path) = desktop {
        info.desktop_stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        info.desktop = KeyFile::read(&path);
    }
    info.icon = resolve_in_image(file, offset, scratch, PathBuf::from(".DirIcon"));
    if info.icon.is_none() {
        let icon_name = info.entry().and_then(|e| e.get("Icon")).map(str::to_owned);
        if let Some(name) = icon_name.filter(|n| !n.contains('/')) {
            for ext in ["png", "svg"] {
                let item = format!("{name}.{ext}");
                if unsquash(file, offset, scratch, &[&item]) {
                    if let Some(found) =
                        resolve_in_image(file, offset, scratch, PathBuf::from(&item))
                    {
                        info.icon = Some(found);
                        break;
                    }
                }
            }
        }
    }
    info
}

/// The id for an AppImage: its desktop file's id, which stays the same
/// across versions, so a newer download replaces the older one and keeps
/// its dock pin. Without a desktop file, the file name without a version.
pub fn id_for(file: &Path, info: &Info) -> String {
    if let Some(stem) = info.desktop_stem.as_deref().filter(|s| !s.is_empty()) {
        return crate::slug(stem);
    }
    crate::slug(&plain_name(file))
}

/// The file name without its version and architecture, for an AppImage
/// that carries no desktop file: `Steam-1.0.0.87-3-anylinux-x86_64` is `Steam`.
pub fn plain_name(file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let words: Vec<&str> = stem
        .split(['-', '_', ' '])
        .take_while(|part| !part.starts_with(|c: char| c.is_ascii_digit()) && *part != "x86")
        .collect();
    if words.is_empty() {
        stem
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf64(shoff: u64, shentsize: u16, shnum: u16, ai: bool) -> Vec<u8> {
        let mut h = vec![0u8; 64];
        h[0..4].copy_from_slice(b"\x7fELF");
        h[4] = 2;
        h[5] = 1;
        if ai {
            h[8..11].copy_from_slice(b"AI\x02");
        }
        h[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
        h[0x3a..0x3c].copy_from_slice(&shentsize.to_le_bytes());
        h[0x3c..0x3e].copy_from_slice(&shnum.to_le_bytes());
        h
    }

    #[test]
    fn offset_is_end_of_section_headers() {
        let h = elf64(186_000, 64, 31, true);
        assert_eq!(squashfs_offset(&h), Some(186_000 + 64 * 31));
        assert_eq!(appimage_type(&h), Some(2));
        assert_eq!(appimage_type(&elf64(0, 0, 0, false)), None);
        assert_eq!(squashfs_offset(b"not an elf"), None);
    }

    #[test]
    fn ids() {
        let mut info = Info::default();
        assert_eq!(
            id_for(
                Path::new("/x/GitHubDesktop-linux-x86_64-3.4.13-linux1.AppImage"),
                &info
            ),
            "githubdesktop-linux"
        );
        assert_eq!(id_for(Path::new("/x/Krita 5.2.AppImage"), &info), "krita");
        assert_eq!(
            plain_name(Path::new("/x/Steam-1.0.0.87-3-anylinux-x86_64.AppImage")),
            "Steam"
        );
        info.desktop_stem = Some("org.kde.krita".into());
        assert_eq!(
            id_for(Path::new("/x/whatever.AppImage"), &info),
            "org.kde.krita"
        );
    }

    #[test]
    fn normalize_stays_inside() {
        assert_eq!(
            normalize(Path::new("usr/share/../share/icons/a.png")),
            Some(PathBuf::from("usr/share/icons/a.png"))
        );
        assert_eq!(normalize(Path::new("../etc/passwd")), None);
    }
}
