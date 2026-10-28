//! `elavo-applications-ctl`: the command line the Nautilus extension, Folio,
//! shortcuts, and tests use, so none of them reads the services' state.
//!
//! ```text
//! elavo-applications-ctl launch <desktop-id>
//! elavo-applications-ctl install <file> [--shared]
//! elavo-applications-ctl remove <bundle>   Trash at home; ask in /Applications
//! elavo-applications-ctl running <bundle>      exit 0 when it is running
//! elavo-applications-ctl shortcut <bundle> <directory>
//! elavo-applications-ctl list
//! elavo-applications-ctl inspect <appimage>
//! ```

use std::path::Path;
use std::process::ExitCode;

use elavo_applications_core::{bundle, running, Bundle, Folder};
use gio::glib;
use gio::prelude::*;

const USAGE: &str = "usage: elavo-applications-ctl launch <id> | install <file> [--shared] | remove <bundle> | running <bundle> | shortcut <bundle> <dir> | list | inspect <appimage>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["launch", id] => launch(id),
        ["install", file] => install(Path::new(file), false),
        ["install", file, "--shared"] | ["install", "--shared", file] => {
            install(Path::new(file), true)
        }
        ["remove", path] => remove(Path::new(path)),
        ["running", path] => {
            return match Bundle::read(Path::new(path)) {
                Some(b) if running::is_running(&b.entry, &b.path) => ExitCode::SUCCESS,
                Some(_) => ExitCode::from(1),
                None => ExitCode::from(2),
            };
        }
        ["shortcut", path, dir] => shortcut(Path::new(path), Path::new(dir)),
        ["list"] => {
            list();
            Ok(())
        }
        ["inspect", file] => {
            inspect(Path::new(file));
            Ok(())
        }
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("elavo-applications-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn all_bundles() -> Vec<Bundle> {
    [Folder::Home, Folder::Shared]
        .into_iter()
        .flat_map(|f| bundle::list(&f.dir()))
        .collect()
}

/// The bundle whose desktop id is `id`, with or without `.desktop`.
fn bundle_for(id: &str) -> Option<Bundle> {
    let id = id.trim_end_matches(".desktop");
    all_bundles()
        .into_iter()
        .find(|b| b.entry.desktop_id().trim_end_matches(".desktop") == id)
}

fn launch(id: &str) -> Result<(), String> {
    let info = match bundle_for(id) {
        Some(b) => gio_unix::DesktopAppInfo::from_filename(b.path.join(bundle::LAUNCH)),
        None => {
            gio_unix::DesktopAppInfo::new(&format!("{}.desktop", id.trim_end_matches(".desktop")))
        }
    };
    let Some(info) = info else {
        let _ = tell_service("missing", id);
        return Err(format!("{id} is not installed"));
    };
    let context = gio::AppLaunchContext::new();
    info.launch(&[], Some(&context)).map_err(|e| e.to_string())
}

/// Asks the running user service to show something.
fn tell_service(action: &str, value: &str) -> Result<(), String> {
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let params = (
        action,
        vec![value.to_variant()],
        std::collections::HashMap::<String, glib::Variant>::new(),
    )
        .to_variant();
    bus.call_sync(
        Some("io.elavo.Applications"),
        "/io/elavo/Applications",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
    )
    .map(|_| ())
    .map_err(|e| format!("Applications is not running ({e})"))
}

fn install(file: &Path, shared: bool) -> Result<(), String> {
    let folder = if shared { Folder::Shared } else { Folder::Home };
    elavo_applications_core::place::place(file, folder)
}

/// Home entries go to the Trash. Shared entries cannot: GIO refuses to
/// trash `/var`, so the service asks Uninstall or Keep and the directory
/// stays until Uninstall.
fn remove(path: &Path) -> Result<(), String> {
    let Some(bundle) = Bundle::read(path) else {
        return Err(format!("{} is not an application", path.display()));
    };
    if bundle.entry.locked {
        return Err(format!(
            "{} is part of Elavo and stays installed",
            bundle.entry.name
        ));
    }
    if Folder::of(path) == Some(Folder::Shared) {
        return tell_service("confirm-remove", &path.to_string_lossy());
    }
    gio::File::for_path(path)
        .trash(gio::Cancellable::NONE)
        .map_err(|e| e.to_string())
}

fn shortcut(path: &Path, dir: &Path) -> Result<(), String> {
    let bundle =
        Bundle::read(path).ok_or_else(|| format!("{} is not an application", path.display()))?;
    let base = elavo_applications_core::file_name_safe(&bundle.entry.name);
    let mut target = dir.join(format!("{base}.desktop"));
    let mut n = 2;
    while target.exists() {
        target = dir.join(format!("{base} {n}.desktop"));
        n += 1;
    }
    bundle::write_file(&target, bundle::shortcut(&bundle).as_bytes(), 0o755)?;
    println!("{}", target.display());
    Ok(())
}

fn list() {
    for b in all_bundles() {
        println!(
            "{}\t{:?}\t{}\t{}",
            b.entry.desktop_id(),
            b.entry.kind,
            b.entry.name,
            b.path.display()
        );
    }
}

fn inspect(file: &Path) {
    let inspected = bundle::inspect_appimage(file);
    println!("id: {}", inspected.id);
    println!("name: {}", inspected.name);
    println!(
        "desktop: {}",
        inspected.info.desktop_stem.as_deref().unwrap_or("-")
    );
    println!(
        "icon: {}",
        inspected
            .info
            .icon
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into())
    );
    if let Some(entry) = inspected.info.entry() {
        for (k, v) in &entry.keys {
            if ["Exec", "Icon", "StartupWMClass", "Categories"].contains(&k.as_str()) {
                println!("  {k}={v}");
            }
        }
    }
}
