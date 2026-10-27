//! Where the folders and exports are.

use std::path::{Path, PathBuf};

use crate::Scope;

/// `ELAVO_SHARED_APPLICATIONS` replaces `/Applications`, for testing on a
/// machine whose image does not have it.
pub fn shared_dir() -> PathBuf {
    std::env::var_os("ELAVO_SHARED_APPLICATIONS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Applications"))
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn home_dir() -> PathBuf {
    home().join("Applications")
}

pub fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"))
}

pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

pub fn state_home() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local/state"))
}

/// Desktop files for AppImages in `~/Applications`.
pub fn user_exports() -> PathBuf {
    data_home().join("applications")
}

/// Desktop files for AppImages in `/Applications`. On `XDG_DATA_DIRS`
/// through the image's environment.d and profile.d files.
pub fn shared_exports() -> PathBuf {
    std::env::var_os("ELAVO_SHARED_EXPORTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/elavo-applications/exports/share/applications"))
}

pub fn writable(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Where the root service remembers which system Flatpaks it has seen.
pub fn system_state() -> PathBuf {
    std::env::var_os("ELAVO_APPLICATIONS_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/elavo-applications"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Folder {
    Shared,
    Home,
}

impl Folder {
    pub fn dir(self) -> PathBuf {
        match self {
            Folder::Shared => shared_dir(),
            Folder::Home => home_dir(),
        }
    }

    /// The Flatpak installation a drop into this folder goes to.
    pub fn scope(self) -> Scope {
        match self {
            Folder::Shared => Scope::System,
            Folder::Home => Scope::User,
        }
    }

    pub fn exports(self) -> PathBuf {
        match self {
            Folder::Shared => shared_exports(),
            Folder::Home => user_exports(),
        }
    }

    /// The folder `path` is directly inside, comparing real paths, since
    /// `/Applications` is a symlink.
    pub fn of(path: &Path) -> Option<Folder> {
        let parent = canonical_parent(path)?;
        [Folder::Shared, Folder::Home]
            .into_iter()
            .find(|f| f.dir().canonicalize().ok().as_deref() == Some(parent.as_path()))
    }
}

/// `path` with its parent directory resolved. The last component is kept as
/// it is, so this works for a path that no longer exists.
pub fn canonical_parent(path: &Path) -> Option<PathBuf> {
    path.parent()?.canonicalize().ok()
}

pub fn canonical(path: &Path) -> Option<PathBuf> {
    Some(canonical_parent(path)?.join(path.file_name()?))
}

/// True when `child` is `dir` or inside it.
pub fn is_inside(child: &Path, dir: &Path) -> bool {
    child.starts_with(dir)
}

pub fn is_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "eapp")
}
