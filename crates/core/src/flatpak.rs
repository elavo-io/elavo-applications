//! The `flatpak` commands the folder runs, and where an installation keeps
//! its exported desktop files and icons.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// What a file installs from.
#[derive(Clone, Debug)]
pub enum Source {
    Ref(PathBuf),
    Bundle(PathBuf),
}

/// `flatpak install` for a file, ready to spawn. Without `--noninteractive`
/// and without a terminal, flatpak prints a line for each progress update,
/// `Installing 1/2… ███ 45%`, which [`progress`] reads. `-y` answers its
/// questions, and polkit may still ask for a password.
pub fn install_command(source: &Source, scope: Scope) -> Command {
    let mut command = Command::new("flatpak");
    command.args(["install", "-y", scope.flag()]);
    match source {
        Source::Ref(path) => command.arg("--from").arg(path),
        Source::Bundle(path) => command.arg("--bundle").arg(path),
    };
    command
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// How far an install is, from 0 to 1, from one line of its output.
pub fn progress(line: &str) -> Option<f64> {
    let mut words = line.split_whitespace();
    if !matches!(
        words.next(),
        Some("Installing" | "Updating" | "Downloading")
    ) {
        return None;
    }
    let step = words.next()?.trim_end_matches('…');
    let (done, total) = step.split_once('/')?;
    let (done, total): (f64, f64) = (done.parse().ok()?, total.parse().ok()?);
    if done < 1.0 || total < done {
        return None;
    }
    let percent = line
        .split_whitespace()
        .rev()
        .find_map(|w| w.strip_suffix('%')?.parse::<f64>().ok())
        .unwrap_or(0.0);
    Some(((done - 1.0 + percent / 100.0) / total).clamp(0.0, 1.0))
}

/// An install that failed because there was nothing to do.
pub fn already_installed(stderr: &str) -> bool {
    stderr.contains("already installed")
}

/// What to tell the person when `flatpak install` failed. `host` is the
/// server it downloads from.
pub fn explain_failure(stderr: &str, host: &str) -> String {
    let lower = stderr.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if says(&[
        "could not resolve",
        "couldn't resolve",
        "name resolution",
        "network is unreachable",
        "no route to host",
        "connection refused",
    ]) {
        format!("Elavo couldn't reach {host}. Check that this computer is connected to the internet, then try again.")
    } else if says(&["timeout was reached", "timed out"]) {
        format!("{host} took too long to respond. Check the internet connection, then try again.")
    } else if says(&["no space left"]) {
        "There isn't enough free space on this computer.".to_owned()
    } else if says(&["not authorized", "authentication", "polkit"]) {
        "Installing for every account on this computer needs an administrator's password."
            .to_owned()
    } else if says(&["gpg", "signature"]) {
        "The download couldn't be verified, so nothing was installed.".to_owned()
    } else {
        stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.strip_prefix("error: ").unwrap_or(l).to_owned())
            .unwrap_or_else(|| "Flatpak stopped without saying why.".to_owned())
    }
}

/// The server a `.flatpakref` installs from, such as `dl.flathub.org`.
pub fn ref_host(file: &Path) -> Option<String> {
    let key = KeyFile::read(file)?;
    let url = key.group("Flatpak Ref")?.get("Url")?.to_owned();
    let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
    rest.split(['/', ':'])
        .next()
        .filter(|h| !h.is_empty())
        .map(str::to_owned)
}

/// A name to show for an application id: `org.gnome.Calculator` is
/// "Calculator".
pub fn display_name(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    let mut chars = last.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => id.to_owned(),
    }
}

/// The application id a `.flatpakref` names.
pub fn ref_name(file: &Path) -> Option<String> {
    let key = KeyFile::read(file)?;
    key.group("Flatpak Ref")?
        .get("Name")
        .map(str::to_owned)
        .filter(|n| !n.is_empty())
}

/// The application id inside a `.flatpak` bundle, such as `com.rtsoft.patchy`.
///
/// `flatpak` has no command that prints this. The file starts with the
/// marker `flatpak`, the constant `0xe5890001`, and a ref
/// `app/<id>/<arch>/<branch>`.
pub fn bundle_name(file: &Path) -> Option<String> {
    let mut head = [0u8; 4096];
    let n = std::fs::File::open(file).ok()?.read(&mut head).ok()?;
    app_id_in_bundle(&head[..n]).map(str::to_owned)
}

fn app_id_in_bundle(head: &[u8]) -> Option<&str> {
    if head.len() < 16 || &head[..8] != b"flatpak\0" || head[8..12] != [0x01, 0x00, 0x89, 0xe5] {
        return None;
    }
    let start = head[12..].windows(4).position(|w| w == b"app/")?;
    let bytes = &head[12 + start..];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let reff = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut parts = reff.split('/');
    if parts.next() != Some("app") {
        return None;
    }
    let id = parts.next()?;
    let arch = parts.next()?;
    let branch = parts.next()?;
    if parts.next().is_some() || arch.is_empty() || branch.is_empty() {
        return None;
    }
    let id_ok = id.contains('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if id_ok {
        Some(id)
    } else {
        None
    }
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

    #[test]
    fn progress_lines() {
        assert_eq!(progress("Installing 1/2…"), Some(0.0));
        assert_eq!(
            progress("Installing 1/2… ████████▏           50%  31.7 kB/s"),
            Some(0.25)
        );
        assert_eq!(progress("Installing 2/2… ████ 100%"), Some(1.0));
        assert_eq!(progress("Configuring https://x as new remote"), None);
        assert_eq!(progress("Installing app/org.x/x86_64/stable"), None);
    }

    #[test]
    fn failures_in_plain_words() {
        let offline = "error: While pulling app/org.x: Could not resolve hostname";
        assert!(explain_failure(offline, "dl.flathub.org")
            .starts_with("Elavo couldn't reach dl.flathub.org."));
        assert!(explain_failure("error: Timeout was reached", "h").starts_with("h took too long"));
        assert_eq!(
            explain_failure("Looking…\nerror: Something odd\n", "h"),
            "Something odd"
        );
        assert!(already_installed(
            "error: org.x/x86_64/stable already installed"
        ));
    }

    #[test]
    fn names_and_hosts() {
        assert_eq!(display_name("org.gnome.Calculator"), "Calculator");
        assert_eq!(display_name("com.rtsoft.patchy"), "Patchy");
        let file = std::env::temp_dir().join(format!("ref-{}.flatpakref", std::process::id()));
        std::fs::write(
            &file,
            "[Flatpak Ref]\nName=org.gnome.Calculator\nUrl=https://dl.flathub.org/repo/\n",
        )
        .unwrap();
        assert_eq!(ref_host(&file).as_deref(), Some("dl.flathub.org"));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn bundle_header_names_the_app() {
        let mut head = b"flatpak\0\x01\x00\x89\xe5".to_vec();
        head.extend_from_slice(b"\0ref\0\0\0\0\0app/com.rtsoft.patchy/x86_64/master\0");
        assert_eq!(app_id_in_bundle(&head), Some("com.rtsoft.patchy"));
        assert_eq!(app_id_in_bundle(b"flatpak\0not-a-bundle"), None);
        assert_eq!(
            app_id_in_bundle(b"flatpak\0\x01\x00\x89\xe5runtime/org.kde.Platform/x86_64/6.11\0"),
            None
        );
    }
}
