//! Creating and refreshing `*.eapp` bundles, and the desktop files they
//! export.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::appimage;
use crate::flatpak;
use crate::icon::{self, Icon};
use crate::keyfile::{exec_arg, KeyFile};
use crate::paths::{self, Folder};
use crate::{Entry, Kind, Scope};

pub const LAUNCH: &str = "launch.desktop";
/// An AppImage's own desktop file, kept so a refresh can rebuild `launch.desktop`.
pub const ORIGINAL: &str = "original.desktop";
pub const RUN: &str = "run";

#[derive(Clone, Debug)]
pub struct Bundle {
    pub path: PathBuf,
    pub entry: Entry,
}

impl Bundle {
    pub fn read(path: &Path) -> Option<Self> {
        Some(Bundle {
            path: path.to_path_buf(),
            entry: Entry::read(path)?,
        })
    }

    pub fn appimage(&self) -> Option<PathBuf> {
        (self.entry.kind == Kind::Appimage && !self.entry.source.is_empty())
            .then(|| self.path.join(&self.entry.source))
    }
}

/// Writes through a temporary file so a reader never sees half a file.
pub fn write_file(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
    let tmp = path.with_extension("tmp~");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every bundle directly in `dir`.
pub fn list(dir: &Path) -> Vec<Bundle> {
    let mut out: Vec<Bundle> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| paths::is_bundle(p) && p.is_dir())
        .filter_map(|p| Bundle::read(&p))
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

pub fn find(dir: &Path, kind: Kind, id: &str) -> Option<Bundle> {
    list(dir)
        .into_iter()
        .find(|b| b.entry.kind == kind && b.entry.id == id)
}

/// `Name.eapp`, or `Name 2.eapp` when that is taken.
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    let base = crate::file_name_safe(name);
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            dir.join(format!("{base}.eapp"))
        } else {
            dir.join(format!("{base} {n}.eapp"))
        };
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

/// Splits the program from its arguments in an `Exec` value.
pub fn split_exec(exec: &str) -> (String, String) {
    let exec = exec.trim();
    if let Some(rest) = exec.strip_prefix('"') {
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    return (rest[..i].to_owned(), rest[i + 1..].trim().to_owned());
                }
                _ => escaped = false,
            }
        }
        return (rest.to_owned(), String::new());
    }
    match exec.split_once(char::is_whitespace) {
        Some((prog, args)) => (prog.to_owned(), args.trim().to_owned()),
        None => (exec.to_owned(), String::new()),
    }
}

fn run_script(appimage: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Written by elavo-applications. Starts the AppImage from its bundle,\n\
         # with the Elavo theme where the AppImage uses the system's GTK.\n\
         cd \"$(dirname \"$0\")\" || exit 1\n\
         if [ -z \"${{GTK_THEME:-}}\" ] && [ -d /usr/share/themes/Elavo ]; then\n\
         \x20   export GTK_THEME=Elavo\n\
         fi\n\
         export XDG_DATA_DIRS=\"${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}\"\n\
         exec ./{} \"$@\"\n",
        shell_quote(appimage)
    )
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

const GTK_VERSION_FLAG: &str = "--gtk-version=4";

/// Brave, Chrome, Chromium, Edge, Opera, and Vivaldi, including channel
/// builds and the AppImage stem (`brave-origin`). Not Firefox, and not
/// Electron: Cursor's id does not match.
fn wants_gtk_version(id: &str) -> bool {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    let lower = id.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "com.google.chrome",
        "com.brave.browser",
        "com.brave.origin",
        "org.chromium.chromium",
        "com.microsoft.edge",
        "com.vivaldi.vivaldi",
        "com.opera.opera",
        "io.github.ungoogled_software.ungoogled_chromium",
        "google-chrome",
        "chromium-browser",
        "chromium",
        "brave-browser",
        "brave-origin",
        "microsoft-edge",
        "vivaldi",
        "opera",
    ];
    lower == "brave"
        || PREFIXES
            .iter()
            .any(|prefix| lower == *prefix || lower.starts_with(&format!("{prefix}.")))
        || PREFIXES.iter().any(|prefix| {
            lower
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('-'))
        })
}

/// Puts [`GTK_VERSION_FLAG`] on an `Exec` line. A line that already chooses
/// a GTK version is left alone.
fn with_gtk_version(exec: &str) -> String {
    if exec
        .split_whitespace()
        .any(|arg| arg == GTK_VERSION_FLAG || arg.starts_with("--gtk-version="))
    {
        return exec.to_owned();
    }
    let (prog, args) = split_exec(exec);
    let base = Path::new(&prog)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(prog.as_str());
    if base == "flatpak" {
        return insert_after_flatpak_id(exec);
    }
    // gtk-launch does not forward arguments to the application.
    if base == "gtk-launch" {
        return exec.to_owned();
    }
    let prog = if exec.trim_start().starts_with('"') {
        exec_arg(&prog)
    } else {
        prog
    };
    if args.is_empty() {
        format!("{prog} {GTK_VERSION_FLAG}")
    } else {
        format!("{prog} {GTK_VERSION_FLAG} {args}")
    }
}

/// `flatpak run [options] <id> [app args]`. The flag belongs to the app.
fn insert_after_flatpak_id(exec: &str) -> String {
    let tokens: Vec<&str> = exec.split_whitespace().collect();
    let Some(run_at) = tokens.iter().position(|token| *token == "run") else {
        return exec.to_owned();
    };
    let Some(id_at) = tokens
        .iter()
        .enumerate()
        .skip(run_at + 1)
        .find(|(_, token)| !token.starts_with('-'))
        .map(|(index, _)| index)
    else {
        return exec.to_owned();
    };
    let mut tokens = tokens;
    tokens.insert(id_at + 1, GTK_VERSION_FLAG);
    tokens.join(" ")
}

/// `launch.desktop` for a bundle at `path`.
pub fn launch_desktop(path: &Path, entry: &Entry) -> KeyFile {
    let mut file = match entry.kind {
        Kind::Flatpak => KeyFile::read(&flatpak::exported_desktop(&entry.id, entry.scope)),
        Kind::System => crate::system::desktop_file(&entry.id).and_then(|p| KeyFile::read(&p)),
        Kind::Appimage => KeyFile::read(&path.join(ORIGINAL)),
    }
    .unwrap_or_default();
    let own_icon = file.entry().and_then(|e| e.get("Icon")).map(str::to_owned);
    if entry.kind == Kind::Appimage {
        let run = exec_arg(&path.join(RUN).to_string_lossy());
        for (_, group) in file.groups.iter_mut() {
            if let Some(exec) = group.get("Exec").map(str::to_owned) {
                let (_, args) = split_exec(&exec);
                let line = if args.is_empty() {
                    run.clone()
                } else {
                    format!("{run} {args}")
                };
                group.set("Exec", &line);
            }
            group.remove("TryExec");
        }
    }
    let main = file.group_mut("Desktop Entry");
    main.set("Type", "Application");
    if main.get("Name").is_none_or(str::is_empty) {
        main.set("Name", &entry.name);
    }
    if main.get("Exec").is_none() {
        let exec = match entry.kind {
            Kind::Flatpak => format!("flatpak run {}", entry.id),
            Kind::Appimage => exec_arg(&path.join(RUN).to_string_lossy()),
            Kind::System => format!("gtk-launch {}", entry.id),
        };
        main.set("Exec", &exec);
    }
    if entry.kind == Kind::Appimage && main.get("StartupWMClass").is_none() {
        main.set("StartupWMClass", &entry.id);
    }
    let icon = icon::resolve(path, &entry.id, own_icon.as_deref());
    main.set("Icon", &icon.desktop_value());
    main.set(
        "X-Elavo-Id",
        entry.desktop_id().trim_end_matches(".desktop"),
    );
    main.set("X-Elavo-Bundle", &path.to_string_lossy());
    main.remove("Hidden");
    main.remove("NoDisplay");
    // Chromium paints its own window unless told to load GTK. Elavo's theme
    // is GTK 4; GTK 3 is only the stopgap. The flag has to follow a Flatpak
    // id, or `flatpak run` treats it as its own option. Firefox is a GTK
    // application and has no such switch. Electron apps are Chromium too,
    // and they do not accept it.
    if wants_gtk_version(&entry.id) {
        for (_, group) in file.groups.iter_mut() {
            if let Some(exec) = group.get("Exec").map(str::to_owned) {
                group.set("Exec", &with_gtk_version(&exec));
            }
        }
    }
    file
}

/// Rewrites `launch.desktop` and, for an AppImage, its `run` script.
pub fn refresh(bundle: &Bundle) -> crate::Result<()> {
    if bundle.entry.kind == Kind::Appimage {
        write_file(
            &bundle.path.join(RUN),
            run_script(&bundle.entry.source).as_bytes(),
            0o755,
        )?;
        // A DwarFS AppImage installed before dwarfsextract was available has
        // no original desktop file and no icon. Read it now, then stop.
        if !bundle.path.join(ORIGINAL).is_file() {
            if let Some(file) = bundle.appimage().filter(|p| appimage::is_dwarfs(p)) {
                let inspected = inspect_appimage(&file);
                let _ = store_appimage(&bundle.path, &inspected.info);
            }
        }
    }
    let text = launch_desktop(&bundle.path, &bundle.entry).render();
    write_file(&bundle.path.join(LAUNCH), text.as_bytes(), 0o644)
}

/// The icon the bundle shows, for the file manager.
pub fn icon(bundle: &Bundle) -> Icon {
    let own = KeyFile::read(&bundle.path.join(LAUNCH))
        .and_then(|f| f.entry().and_then(|e| e.get("Icon")).map(str::to_owned));
    match own {
        Some(v) if v.starts_with('/') => Icon::File(PathBuf::from(v)),
        Some(v) if !v.is_empty() => Icon::Name(v),
        _ => Icon::Name(icon::FALLBACK.to_owned()),
    }
}

/// What a dropped AppImage would become, before anything moves.
pub struct Inspected {
    pub id: String,
    pub name: String,
    pub info: appimage::Info,
    scratch: PathBuf,
}

impl Drop for Inspected {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

pub fn inspect_appimage(file: &Path) -> Inspected {
    let scratch = std::env::temp_dir().join(format!(
        "elavo-applications-{}-{}",
        std::process::id(),
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::create_dir_all(&scratch);
    let info = appimage::read(file, &scratch);
    let id = appimage::id_for(file, &info);
    let name = info.name().unwrap_or_else(|| appimage::plain_name(file));
    Inspected {
        id,
        name,
        info,
        scratch,
    }
}

/// Moves a dropped AppImage into a new bundle. `replace` is an existing
/// bundle of the same app, which keeps its directory, custom icon, and
/// name; its old AppImage is deleted.
pub fn install_appimage(
    folder: Folder,
    file: &Path,
    inspected: &Inspected,
    replace: Option<&Bundle>,
) -> crate::Result<Bundle> {
    let dir = folder.dir();
    let path = match replace {
        Some(b) => b.path.clone(),
        None => free_path(&dir, &inspected.name),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let source = crate::file_name_safe(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.AppImage", inspected.id)),
    );
    if let Some(old) = replace.and_then(Bundle::appimage) {
        if old.file_name().map(|n| n.to_string_lossy().into_owned()) != Some(source.clone()) {
            let _ = std::fs::remove_file(old);
        }
    }
    let target = path.join(&source);
    if std::fs::rename(file, &target).is_err() {
        std::fs::copy(file, &target).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(file);
    }
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    store_appimage(&path, &inspected.info)?;
    let entry = Entry {
        kind: Kind::Appimage,
        id: inspected.id.clone(),
        name: replace
            .map(|b| b.entry.name.clone())
            .unwrap_or_else(|| inspected.name.clone()),
        scope: folder.scope(),
        origin: String::new(),
        source,
        locked: false,
    };
    entry.write(&path)?;
    let bundle = Bundle { path, entry };
    refresh(&bundle)?;
    Ok(bundle)
}

/// Copies the desktop file and icon `read` found into the bundle.
fn store_appimage(dir: &Path, info: &appimage::Info) -> crate::Result<()> {
    if let Some(desktop) = &info.desktop {
        write_file(&dir.join(ORIGINAL), desktop.render().as_bytes(), 0o644)?;
    }
    if let Some(icon) = &info.icon {
        let name = if icon.extension().is_some_and(|e| e == "svg") || sniff_svg(icon) {
            "icon.svg"
        } else {
            "icon.png"
        };
        for old in icon::BUNDLED {
            let _ = std::fs::remove_file(dir.join(old));
        }
        let _ = std::fs::copy(icon, dir.join(name));
    }
    Ok(())
}

fn sniff_svg(path: &Path) -> bool {
    std::fs::read(path)
        .map(|b| {
            let head = String::from_utf8_lossy(&b[..b.len().min(512)]).to_lowercase();
            head.contains("<svg") || head.starts_with("<?xml")
        })
        .unwrap_or(false)
}

/// The entry for an installed Flatpak, in `dir`.
pub fn create_flatpak(dir: &Path, app: &flatpak::Installed, scope: Scope) -> crate::Result<Bundle> {
    let path = free_path(dir, &app.name);
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    if let Some(icon) = flatpak::exported_icon(&app.id, scope) {
        let name = if icon.extension().is_some_and(|e| e == "svg") {
            "icon.svg"
        } else {
            "icon.png"
        };
        let _ = std::fs::copy(icon, path.join(name));
    }
    let entry = Entry {
        kind: Kind::Flatpak,
        id: app.id.clone(),
        name: app.name.clone(),
        scope,
        origin: app.origin.clone(),
        source: String::new(),
        locked: false,
    };
    entry.write(&path)?;
    let bundle = Bundle { path, entry };
    refresh(&bundle)?;
    Ok(bundle)
}

/// A locked entry for a system application. Root writes these; the bundle
/// is left `0755` and root-owned so no account can move it to the Trash.
pub fn create_system(dir: &Path, id: &str) -> crate::Result<Bundle> {
    let desktop =
        crate::system::desktop_file(id).ok_or_else(|| format!("{id}: no desktop file"))?;
    let file = KeyFile::read(&desktop).unwrap_or_default();
    let name = file
        .entry()
        .and_then(|e| e.get("Name"))
        .unwrap_or(id)
        .to_owned();
    let path = match find(dir, Kind::System, id) {
        Some(existing) => existing.path,
        None => free_path(dir, &name),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let entry = Entry {
        kind: Kind::System,
        id: id.to_owned(),
        name,
        scope: Scope::System,
        origin: String::new(),
        source: String::new(),
        locked: true,
    };
    entry.write(&path)?;
    let bundle = Bundle { path, entry };
    refresh(&bundle)?;
    let _ = std::fs::set_permissions(&bundle.path, std::fs::Permissions::from_mode(0o755));
    Ok(bundle)
}

/// Writes the exported desktop files for every AppImage in `folder`, and
/// removes exports whose bundle is gone.
pub fn sync_exports(folder: Folder) -> crate::Result<()> {
    let exports = folder.exports();
    std::fs::create_dir_all(&exports).map_err(|e| format!("{}: {e}", exports.display()))?;
    let bundles: Vec<Bundle> = list(&folder.dir())
        .into_iter()
        .filter(|b| b.entry.kind == Kind::Appimage)
        .collect();
    let wanted: Vec<String> = bundles.iter().map(|b| b.entry.desktop_id()).collect();
    for e in std::fs::read_dir(&exports).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("elavo-appimage-") && !wanted.contains(&name) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    for b in &bundles {
        let text = launch_desktop(&b.path, &b.entry).render();
        let target = exports.join(b.entry.desktop_id());
        if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
            write_file(&target, text.as_bytes(), 0o644)?;
        }
    }
    Ok(())
}

/// A shortcut `.desktop` file for the bundle, for a drag out of Applications.
pub fn shortcut(bundle: &Bundle) -> String {
    let mut file = KeyFile::default();
    let main = file.group_mut("Desktop Entry");
    main.set("Type", "Application");
    main.set("Name", &bundle.entry.name);
    let id = bundle.entry.desktop_id();
    let id = id.trim_end_matches(".desktop");
    main.set(
        "Exec",
        &format!("elavo-applications-ctl launch {}", exec_arg(id)),
    );
    main.set("Icon", &icon(bundle).desktop_value());
    main.set("X-Elavo-Kind", "shortcut");
    main.set("X-Elavo-Id", id);
    file.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_split() {
        assert_eq!(split_exec("krita %F"), ("krita".into(), "%F".into()));
        assert_eq!(
            split_exec("\"/opt/my app/bin\" --x %U"),
            ("/opt/my app/bin".into(), "--x %U".into())
        );
        assert_eq!(split_exec("AppRun"), ("AppRun".into(), String::new()));
    }

    #[test]
    fn free_names() {
        let dir = std::env::temp_dir().join(format!("eapp-free-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Krita.eapp")).unwrap();
        assert_eq!(free_path(&dir, "Krita"), dir.join("Krita 2.eapp"));
        assert_eq!(free_path(&dir, "AC/DC"), dir.join("AC-DC.eapp"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appimage_launch_runs_the_wrapper() {
        let dir =
            std::env::temp_dir().join(format!("eapp-launch-{}/Krita.eapp", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ORIGINAL),
            "[Desktop Entry]\nName=Krita\nExec=krita %F\nTryExec=krita\nIcon=krita\n\n[Desktop Action new]\nName=New\nExec=krita --new\n",
        )
        .unwrap();
        let entry = Entry {
            kind: Kind::Appimage,
            id: "org.kde.krita".into(),
            name: "Krita".into(),
            scope: Scope::User,
            origin: String::new(),
            source: "krita.AppImage".into(),
            locked: false,
        };
        let file = launch_desktop(&dir, &entry);
        let main = file.entry().unwrap();
        let run = dir.join(RUN).to_string_lossy().into_owned();
        assert_eq!(main.get("Exec"), Some(format!("{run} %F").as_str()));
        assert_eq!(main.get("TryExec"), None);
        assert_eq!(main.get("X-Elavo-Id"), Some("elavo-appimage-org.kde.krita"));
        assert_eq!(main.get("StartupWMClass"), Some("org.kde.krita"));
        assert_eq!(
            file.group("Desktop Action new").unwrap().get("Exec"),
            Some(format!("{run} --new").as_str())
        );
        assert!(run_script("it's.AppImage").contains("exec ./'it'\\''s.AppImage' \"$@\""));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn chromium_launchers_ask_for_gtk4_and_firefox_does_not() {
        assert!(wants_gtk_version("com.google.Chrome"));
        assert!(wants_gtk_version("com.brave.Browser"));
        assert!(wants_gtk_version("brave-origin"));
        assert!(wants_gtk_version("google-chrome-unstable"));
        assert!(wants_gtk_version("com.microsoft.Edge"));
        assert!(!wants_gtk_version("org.mozilla.firefox"));
        assert!(!wants_gtk_version("cursor"));

        let chrome = "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=/app/bin/chrome --file-forwarding com.google.Chrome @@u %U @@";
        assert_eq!(
            with_gtk_version(chrome),
            "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=/app/bin/chrome --file-forwarding com.google.Chrome --gtk-version=4 @@u %U @@"
        );
        assert_eq!(
            with_gtk_version("flatpak run --command=brave com.brave.Browser --incognito"),
            "flatpak run --command=brave com.brave.Browser --gtk-version=4 --incognito"
        );
        assert_eq!(
            with_gtk_version("\"/Applications/Brave Origin.eapp/run\" %U"),
            "\"/Applications/Brave Origin.eapp/run\" --gtk-version=4 %U"
        );
        let chosen = "google-chrome --gtk-version=3 %U";
        assert_eq!(with_gtk_version(chosen), chosen);
        assert_eq!(
            with_gtk_version("gtk-launch com.google.Chrome"),
            "gtk-launch com.google.Chrome"
        );
    }
}
