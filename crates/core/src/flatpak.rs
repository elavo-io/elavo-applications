//! The `flatpak` commands the folder runs, and where an installation keeps
//! its exported desktop files and icons.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::keyfile::KeyFile;
use crate::Scope;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub origin: String,
    pub name: String,
}

pub fn parse_list(text: &str) -> Vec<Installed> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim();
            if id.is_empty() {
                return None;
            }
            let origin = cols.next().unwrap_or("").trim();
            let name = cols.next().unwrap_or("").trim();
            Some(Installed {
                id: id.to_owned(),
                origin: origin.to_owned(),
                name: if name.is_empty() { id } else { name }.to_owned(),
            })
        })
        .collect()
}

fn output(args: &[&str]) -> crate::Result<String> {
    let out = Command::new("flatpak")
        .args(args)
        .output()
        .map_err(|e| format!("flatpak: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            format!("flatpak {} failed", args.join(" "))
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Applications in one installation. None when `flatpak` could not be run,
/// so a failure is not mistaken for "nothing is installed".
pub fn list(scope: Scope) -> Option<Vec<Installed>> {
    output(&[
        "list",
        "--app",
        scope.flag(),
        "--columns=application,origin,name",
    ])
    .ok()
    .map(|t| parse_list(&t))
}

pub fn is_installed(id: &str, scope: Scope) -> bool {
    output(&["info", scope.flag(), id]).is_ok()
}

pub fn install_ref(file: &Path, scope: Scope) -> crate::Result<()> {
    let path = file.to_string_lossy();
    output(&[
        "install",
        "--noninteractive",
        "-y",
        scope.flag(),
        "--from",
        &path,
    ])
    .map(drop)
}

pub fn install_bundle(file: &Path, scope: Scope) -> crate::Result<()> {
    let path = file.to_string_lossy();
    output(&[
        "install",
        "--noninteractive",
        "-y",
        scope.flag(),
        "--bundle",
        &path,
    ])
    .map(drop)
}

pub fn install_from(origin: &str, id: &str, scope: Scope) -> crate::Result<()> {
    output(&[
        "install",
        "--noninteractive",
        "-y",
        scope.flag(),
        origin,
        id,
    ])
    .map(drop)
}

/// Keeps `~/.var/app/<id>`, so reinstalling restores the app's state.
pub fn uninstall(id: &str, scope: Scope) -> crate::Result<()> {
    output(&["uninstall", "--noninteractive", "-y", scope.flag(), id]).map(drop)
}

/// The application id a `.flatpakref` names.
pub fn ref_name(file: &Path) -> Option<String> {
    let key = KeyFile::read(file)?;
    key.group("Flatpak Ref")?
        .get("Name")
        .map(str::to_owned)
        .filter(|n| !n.is_empty())
}

/// The application id inside a `.flatpak` bundle.
pub fn bundle_name(file: &Path) -> Option<String> {
    let path = file.to_string_lossy();
    let text = output(&["info", "--show-ref", "--file", &path])
        .or_else(|_| output(&["build-bundle-info", &path]))
        .ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("app/"))
        .and_then(|r| r.split('/').next())
        .map(str::to_owned)
}

pub fn installation(scope: Scope) -> PathBuf {
    match scope {
        Scope::User => crate::paths::data_home().join("flatpak"),
        Scope::System => PathBuf::from("/var/lib/flatpak"),
    }
}

/// The directory whose entries change when an application is installed or
/// removed. The services watch it.
pub fn app_dir(scope: Scope) -> PathBuf {
    installation(scope).join("app")
}

pub fn exported_desktop(id: &str, scope: Scope) -> PathBuf {
    installation(scope)
        .join("exports/share/applications")
        .join(format!("{id}.desktop"))
}

/// The largest exported icon, scalable first.
pub fn exported_icon(id: &str, scope: Scope) -> Option<PathBuf> {
    let hicolor = installation(scope).join("exports/share/icons/hicolor");
    let scalable = hicolor.join(format!("scalable/apps/{id}.svg"));
    if scalable.is_file() {
        return Some(scalable);
    }
    let mut best: Option<(u32, PathBuf)> = None;
    for dir in std::fs::read_dir(&hicolor).into_iter().flatten().flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        let Some(size) = name.split('x').next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let png = dir.path().join(format!("apps/{id}.png"));
        if png.is_file() && best.as_ref().is_none_or(|(b, _)| size > *b) {
            best = Some((size, png));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_columns() {
        let got = parse_list("org.mozilla.firefox\tflathub\tFirefox\ncom.x.Y\tflathub\t\n\n");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "Firefox");
        assert_eq!(got[1].name, "com.x.Y");
    }
}
