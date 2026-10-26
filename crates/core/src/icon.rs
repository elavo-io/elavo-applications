//! Which icon a bundle shows: the person's own file, then the Elavo theme,
//! then the icon bundled with the application, then the generic glyph.
//! The template composite replaces the bundled icon once the template exists.

use std::path::{Path, PathBuf};

pub const CUSTOM: [&str; 3] = ["custom-icon.png", "custom-icon.svg", "custom-icon.webp"];
pub const BUNDLED: [&str; 2] = ["icon.png", "icon.svg"];
pub const FALLBACK: &str = "application-x-executable";
const THEME: &str = "Elavo";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    File(PathBuf),
    Name(String),
}

impl Icon {
    /// The value for a desktop file's `Icon=`.
    pub fn desktop_value(&self) -> String {
        match self {
            Icon::File(p) => p.to_string_lossy().into_owned(),
            Icon::Name(n) => n.clone(),
        }
    }
}

pub fn custom(bundle: &Path) -> Option<PathBuf> {
    CUSTOM.iter().map(|n| bundle.join(n)).find(|p| p.is_file())
}

pub fn bundled(bundle: &Path) -> Option<PathBuf> {
    BUNDLED.iter().map(|n| bundle.join(n)).find(|p| p.is_file())
}

fn theme_roots() -> Vec<PathBuf> {
    vec![
        crate::paths::data_home().join("icons").join(THEME),
        PathBuf::from("/usr/share/icons").join(THEME),
    ]
}

/// Whether the Elavo icon theme draws `name`.
pub fn theme_has(name: &str) -> bool {
    if name.is_empty() || name.contains('/') {
        return false;
    }
    fn walk(dir: &Path, name: &str, depth: u8) -> bool {
        if depth > 3 {
            return false;
        }
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                if walk(&path, name, depth + 1) {
                    return true;
                }
            } else if path.file_stem().is_some_and(|s| s == name)
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|d| d == "apps")
            {
                return true;
            }
        }
        false
    }
    theme_roots().iter().any(|r| walk(r, name, 0))
}

/// `id` is the application's id. `own` is the `Icon=` of the application's
/// own desktop file: a theme name or a path. A theme name there is still
/// drawn from another theme (hicolor) when Elavo's has none.
pub fn resolve(bundle: &Path, id: &str, own: Option<&str>) -> Icon {
    if let Some(path) = custom(bundle) {
        return Icon::File(path);
    }
    if let Some(name) = [Some(id), own].into_iter().flatten().find(|n| theme_has(n)) {
        return Icon::Name(name.to_owned());
    }
    if let Some(path) = bundled(bundle) {
        return Icon::File(path);
    }
    match own.filter(|n| !n.is_empty()) {
        Some(n) if n.starts_with('/') && Path::new(n).is_file() => Icon::File(PathBuf::from(n)),
        Some(n) if !n.contains('/') => Icon::Name(n.to_owned()),
        _ => Icon::Name(FALLBACK.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_wins_then_bundled_then_fallback() {
        let dir = std::env::temp_dir().join(format!("eapp-icon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            resolve(&dir, "no.such.app.x", None),
            Icon::Name(FALLBACK.into())
        );
        assert_eq!(
            resolve(&dir, "no.such.app.x", Some("krita")),
            Icon::Name("krita".into())
        );
        std::fs::write(dir.join("icon.png"), b"png").unwrap();
        assert_eq!(
            resolve(&dir, "no.such.app.x", Some("krita")),
            Icon::File(dir.join("icon.png"))
        );
        std::fs::write(dir.join("custom-icon.png"), b"png").unwrap();
        assert_eq!(
            resolve(&dir, "no.such.app.x", None),
            Icon::File(dir.join("custom-icon.png"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
