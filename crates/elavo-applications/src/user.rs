//! The account's service: installs what this account drops into an
//! Applications folder, keeps `~/Applications` in step with the account's
//! Flatpaks, asks Uninstall or Keep when an entry reaches the Trash, and
//! gives every entry its icon in Files.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use elavo_applications_core::reconcile::{self, Drop};
use elavo_applications_core::{
    bundle, flatpak, icon, paths, running, trash, Bundle, Entry, Folder, Kind,
};
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::prompt::{Prompt, Style};

const APP_ID: &str = "io.elavo.Applications";
/// Files the service writes inside a bundle. Anything else that appears
/// there is a picture dropped onto the entry.
const OWN_FILES: [&str; 7] = [
    "entry.json",
    "launch.desktop",
    "original.desktop",
    "run",
    "icon.png",
    "icon.svg",
    icon::STANDARD,
];

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::IS_SERVICE)
        .build();
    let started = Cell::new(false);
    let service: Rc<RefCell<Option<Rc<Service>>>> = Rc::default();
    let slot = service.clone();
    app.connect_startup(move |app| {
        if started.replace(true) {
            return;
        }
        let s = Service::new(app);
        s.start();
        slot.replace(Some(s));
    });
    app.connect_activate(|_| {});
    let code = app.run_with_args::<&str>(&[]);
    drop(service);
    code
}

fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn sleep(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    gio::spawn_blocking(f)
        .await
        .expect("worker thread panicked")
}

struct Service {
    _hold: gio::ApplicationHoldGuard,
    prompt: Rc<Prompt>,
    /// Every bundle in both folders, by the path it has in its folder.
    known: RefCell<HashMap<PathBuf, Entry>>,
    /// Bundles this service moved or removed itself.
    expected_gone: RefCell<HashSet<PathBuf>>,
    /// Flatpak ids being installed or removed.
    busy: RefCell<HashSet<String>>,
    /// Files already reported as not installable.
    told: RefCell<HashSet<PathBuf>>,
    /// Dropped files being installed.
    handling: RefCell<HashSet<PathBuf>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    bundle_monitors: RefCell<HashMap<PathBuf, gio::FileMonitor>>,
    queued: RefCell<HashSet<PathBuf>>,
    timer: Cell<bool>,
    mirror_queued: Cell<bool>,
}

impl Service {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let service = Rc::new(Service {
            _hold: app.hold(),
            prompt: Prompt::new(app),
            known: RefCell::default(),
            expected_gone: RefCell::default(),
            busy: RefCell::default(),
            told: RefCell::default(),
            handling: RefCell::default(),
            monitors: RefCell::default(),
            bundle_monitors: RefCell::default(),
            queued: RefCell::default(),
            timer: Cell::new(false),
            mirror_queued: Cell::new(false),
        });
        let missing = gio::SimpleAction::new("missing", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&service);
        missing.connect_activate(move |_, param| {
            let (Some(s), Some(id)) = (weak.upgrade(), param.and_then(|p| p.str())) else {
                return;
            };
            s.prompt.tell(
                "This application is no longer installed",
                &format!(
                    "{id} was uninstalled. Delete the shortcut, or install the application again."
                ),
            );
        });
        app.add_action(&missing);
        // The Dock refuses to trash an application with windows and asks
        // this service to say why.
        let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&service);
        open.connect_activate(move |_, param| {
            let (Some(s), Some(name)) = (weak.upgrade(), param.and_then(|p| p.str())) else {
                return;
            };
            s.prompt.tell(
                &format!("“{name}” is open"),
                &format!("Quit {name}, then move it to the Trash."),
            );
        });
        app.add_action(&open);
        service
    }

    fn folders() -> Vec<Folder> {
        [Folder::Home, Folder::Shared]
            .into_iter()
            .filter(|f| f.dir().is_dir())
            .collect()
    }

    fn folder_of(path: &Path) -> Option<Folder> {
        let parent = path.parent()?;
        [Folder::Home, Folder::Shared]
            .into_iter()
            .find(|f| f.dir() == parent)
    }

    fn start(self: &Rc<Self>) {
        let _ = std::fs::create_dir_all(Folder::Home.dir());
        seed_bookmarks();
        for folder in Self::folders() {
            self.watch_folder(folder);
            for b in bundle::list(&folder.dir()) {
                if reconcile::owner(&b.path) == Some(uid()) {
                    let _ = bundle::refresh(&b);
                    self.watch_bundle(&b.path);
                }
                set_file_icon(&b);
                self.known.borrow_mut().insert(b.path.clone(), b.entry);
            }
            for e in std::fs::read_dir(folder.dir())
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = e.path();
                if !paths::is_bundle(&path) && reconcile::owner(&path) == Some(uid()) {
                    self.queue(path);
                }
            }
        }
        let _ = bundle::sync_exports(Folder::Home);
        let _ = bundle::sync_exports(Folder::Shared);
        self.watch_flatpaks();
        self.mirror();
    }

    fn watch_folder(self: &Rc<Self>, folder: Folder) {
        let file = gio::File::for_path(folder.dir());
        let Ok(monitor) =
            file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, file, other, event| {
            let Some(s) = weak.upgrade() else { return };
            use gio::FileMonitorEvent as E;
            match event {
                E::Created | E::Deleted | E::MovedIn | E::MovedOut | E::ChangesDoneHint => {
                    if let Some(p) = file.path() {
                        s.queue(p);
                    }
                }
                E::Renamed => {
                    for f in [Some(file), other].into_iter().flatten() {
                        if let Some(p) = f.path() {
                            s.queue(p);
                        }
                    }
                }
                _ => {}
            }
        });
        self.monitors.borrow_mut().push(monitor);
    }

    /// A picture dropped onto an entry lands inside its directory.
    fn watch_bundle(self: &Rc<Self>, path: &Path) {
        if self.bundle_monitors.borrow().contains_key(path) {
            return;
        }
        let Ok(monitor) = gio::File::for_path(path)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        let bundle_path = path.to_path_buf();
        monitor.connect_changed(move |_, file, other, event| {
            let Some(s) = weak.upgrade() else { return };
            use gio::FileMonitorEvent as E;
            let target = match event {
                E::Renamed => other.and_then(|o| o.path()),
                E::Created | E::MovedIn | E::Deleted | E::MovedOut | E::ChangesDoneHint => {
                    file.path()
                }
                _ => None,
            };
            let Some(target) = target else { return };
            let name = display_name(&target);
            if name.ends_with("~") || name.starts_with('.') || OWN_FILES.contains(&name.as_str()) {
                return;
            }
            let bundle_path = bundle_path.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                s.picture_dropped(&bundle_path, &target);
            });
        });
        self.bundle_monitors
            .borrow_mut()
            .insert(path.to_path_buf(), monitor);
    }

    fn watch_flatpaks(self: &Rc<Self>) {
        let dir = flatpak::app_dir(elavo_applications_core::Scope::User);
        let _ = std::fs::create_dir_all(&dir);
        let Ok(monitor) = gio::File::for_path(dir)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(s) = weak.upgrade() {
                s.queue_mirror();
            }
        });
        self.monitors.borrow_mut().push(monitor);
    }

    fn queue(self: &Rc<Self>, path: PathBuf) {
        self.queued.borrow_mut().insert(path);
        if self.timer.replace(true) {
            return;
        }
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            this.timer.set(false);
            let paths: Vec<PathBuf> = this.queued.borrow_mut().drain().collect();
            for p in paths {
                this.process(p);
            }
        });
    }

    fn queue_mirror(self: &Rc<Self>) {
        if self.mirror_queued.replace(true) {
            return;
        }
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            this.mirror_queued.set(false);
            this.mirror();
        });
    }

    fn process(self: &Rc<Self>, path: PathBuf) {
        let Some(folder) = Self::folder_of(&path) else {
            return;
        };
        let exists = std::fs::symlink_metadata(&path).is_ok();
        if exists && paths::is_bundle(&path) && path.is_dir() {
            if !self.known.borrow().contains_key(&path) {
                self.appeared(folder, path);
            }
            return;
        }
        if exists {
            if reconcile::owner(&path) == Some(uid()) {
                let this = self.clone();
                glib::spawn_future_local(async move { this.dropped(folder, path).await });
            }
            return;
        }
        let gone = self.known.borrow_mut().remove(&path);
        if let Some(entry) = gone {
            self.bundle_monitors.borrow_mut().remove(&path);
            if self.expected_gone.borrow_mut().remove(&path) {
                return;
            }
            let this = self.clone();
            glib::spawn_future_local(async move { this.departed(folder, path, entry).await });
        }
    }

    fn after_install(self: &Rc<Self>, folder: Folder, b: &Bundle) {
        self.known
            .borrow_mut()
            .insert(b.path.clone(), b.entry.clone());
        self.watch_bundle(&b.path);
        set_file_icon(b);
        let _ = bundle::sync_exports(folder);
    }

    async fn dropped(self: Rc<Self>, folder: Folder, path: PathBuf) {
        if !self.handling.borrow_mut().insert(path.clone()) {
            return;
        }
        if wait_settled(&path).await {
            self.handle_drop(folder, path.clone()).await;
        }
        self.handling.borrow_mut().remove(&path);
    }

    async fn handle_drop(self: &Rc<Self>, folder: Folder, path: PathBuf) {
        match reconcile::classify(&path) {
            Drop::Ignore => {}
            Drop::Other => {
                if self.told.borrow_mut().insert(path.clone()) {
                    self.prompt.tell(
                        &format!("“{}” stays where it is", display_name(&path)),
                        "Elavo installs AppImages and Flatpaks from the Applications folder. Other files are left as they are.",
                    );
                }
            }
            Drop::AppImage => self.install_appimage(folder, path).await,
            Drop::FlatpakRef | Drop::FlatpakBundle => self.install_flatpak(folder, path).await,
        }
    }

    async fn install_appimage(self: &Rc<Self>, folder: Folder, path: PathBuf) {
        let p = path.clone();
        let inspected = blocking(move || bundle::inspect_appimage(&p)).await;
        let existing = bundle::find(&folder.dir(), Kind::Appimage, &inspected.id);
        if let Some(old) = &existing {
            if running::is_running(&old.entry, &old.path) {
                self.prompt.tell(
                    &format!("“{}” is open", old.entry.name),
                    &format!(
                        "Quit {}, then drop the new version into Applications again.",
                        old.entry.name
                    ),
                );
                return;
            }
            let answer = self
                .prompt
                .ask(
                    &format!("Replace “{}”?", old.entry.name),
                    &format!(
                        "{} is already in Applications. Replacing it keeps its place in the Dock and its own icon.",
                        old.entry.name
                    ),
                    &[("Cancel", Style::Plain), ("Replace", Style::Suggested)],
                )
                .await;
            if answer != Some(1) {
                return;
            }
        }
        let p = path.clone();
        let result = blocking(move || {
            let r = bundle::install_appimage(folder, &p, &inspected, existing.as_ref());
            drop(inspected);
            r
        })
        .await;
        match result {
            Ok(b) => self.after_install(folder, &b),
            Err(e) => self.prompt.tell(
                &format!("“{}” could not be installed", display_name(&path)),
                &e,
            ),
        }
    }

    async fn install_flatpak(self: &Rc<Self>, folder: Folder, path: PathBuf) {
        let scope = folder.scope();
        let p = path.clone();
        let is_ref = reconcile::classify(&path) == Drop::FlatpakRef;
        let id = blocking(move || {
            if is_ref {
                flatpak::ref_name(&p)
            } else {
                flatpak::bundle_name(&p)
            }
        })
        .await;
        let Some(id) = id else {
            self.prompt.tell(
                &format!("“{}” could not be read", display_name(&path)),
                "It is not a Flatpak reference or bundle Elavo can install.",
            );
            return;
        };
        if !self.busy.borrow_mut().insert(id.clone()) {
            return;
        }
        let shown = id.rsplit('.').next().unwrap_or(&id).to_owned();
        let notice = self.clone();
        let notice_name = shown.clone();
        glib::spawn_future_local(async move {
            notice.prompt.tell(
                &format!("Installing {notice_name}…"),
                "It appears in Applications when it is ready.",
            );
        });
        let p = path.clone();
        let install_id = id.clone();
        let result = blocking(move || {
            if flatpak::is_installed(&install_id, scope) {
                return Ok(());
            }
            if is_ref {
                flatpak::install_ref(&p, scope)
            } else {
                flatpak::install_bundle(&p, scope)
            }
        })
        .await;
        self.busy.borrow_mut().remove(&id);
        if let Err(e) = result {
            self.prompt
                .tell(&format!("{shown} could not be installed"), &e);
            return;
        }
        let _ = std::fs::remove_file(&path);
        let dir = folder.dir();
        // The root service makes shared entries when the system Flatpaks
        // change. Making one here too would race it into a duplicate.
        if folder == Folder::Shared {
            for _ in 0..20 {
                if let Some(b) = bundle::find(&dir, Kind::Flatpak, &id) {
                    self.after_install(folder, &b);
                    return;
                }
                sleep(500).await;
            }
        }
        let made = blocking(move || {
            if let Some(b) = bundle::find(&dir, Kind::Flatpak, &id) {
                return Ok(b);
            }
            let app = flatpak::list(scope)
                .unwrap_or_default()
                .into_iter()
                .find(|a| a.id == id)
                .unwrap_or(flatpak::Installed {
                    id: id.clone(),
                    origin: String::new(),
                    name: id.clone(),
                });
            bundle::create_flatpak(&dir, &app, scope)
        })
        .await;
        match made {
            Ok(b) => self.after_install(folder, &b),
            Err(e) => self.prompt.tell(
                "The application was installed, but its entry could not be made",
                &e,
            ),
        }
    }

    fn appeared(self: &Rc<Self>, folder: Folder, path: PathBuf) {
        let Some(b) = Bundle::read(&path) else {
            return;
        };
        let mine = reconcile::owner(&path) == Some(uid());
        if b.entry.kind == Kind::Flatpak && b.entry.scope != folder.scope() && mine {
            let other = if folder == Folder::Home {
                Folder::Shared
            } else {
                Folder::Home
            };
            let back = bundle::free_path(&other.dir(), &b.entry.name);
            self.expected_gone.borrow_mut().insert(path.clone());
            if std::fs::rename(&path, &back).is_ok() {
                let where_ = if other == Folder::Shared {
                    "Applications, for every account"
                } else {
                    "your own Applications folder"
                };
                self.prompt.tell(
                    &format!("“{}” stays in {where_}", b.entry.name),
                    "A Flatpak is installed either for every account or for yours. Moving it between the two Applications folders is not supported yet.",
                );
                return;
            }
            self.expected_gone.borrow_mut().remove(&path);
        }
        self.known
            .borrow_mut()
            .insert(path.clone(), b.entry.clone());
        if mine {
            self.watch_bundle(&path);
        }
        set_file_icon(&b);
        match b.entry.kind {
            Kind::Appimage if mine => {
                let _ = bundle::refresh(&b);
                let _ = bundle::sync_exports(folder);
            }
            Kind::Flatpak if mine && !b.entry.origin.is_empty() => {
                let this = self.clone();
                glib::spawn_future_local(async move { this.reinstall(b).await });
            }
            _ => {}
        }
    }

    /// A Flatpak entry came back from the Trash after it was uninstalled.
    async fn reinstall(self: &Rc<Self>, b: Bundle) {
        let (id, scope, origin) = (b.entry.id.clone(), b.entry.scope, b.entry.origin.clone());
        let check = id.clone();
        if blocking(move || flatpak::is_installed(&check, scope)).await {
            let _ = bundle::refresh(&b);
            return;
        }
        if !self.busy.borrow_mut().insert(id.clone()) {
            return;
        }
        self.prompt.tell(
            &format!("Reinstalling {}…", b.entry.name),
            "It was put back from the Trash. It can be opened again once it has downloaded.",
        );
        let install_id = id.clone();
        let result = blocking(move || flatpak::install_from(&origin, &install_id, scope)).await;
        self.busy.borrow_mut().remove(&id);
        match result {
            Ok(()) => {
                let _ = bundle::refresh(&b);
                set_file_icon(&b);
            }
            Err(e) => self
                .prompt
                .tell(&format!("{} could not be reinstalled", b.entry.name), &e),
        }
    }

    fn picture_dropped(self: &Rc<Self>, bundle_path: &Path, picture: &Path) {
        let Some(b) = Bundle::read(bundle_path) else {
            return;
        };
        let name = display_name(picture);
        let is_custom = icon::CUSTOM.contains(&name.as_str());
        if picture.is_file() && !is_custom && Some(name.as_str()) != Some(b.entry.source.as_str()) {
            let ext = picture
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let target = match ext.as_str() {
                "png" => "custom-icon.png",
                "svg" => "custom-icon.svg",
                "webp" => "custom-icon.webp",
                _ => return,
            };
            for old in icon::CUSTOM {
                let _ = std::fs::remove_file(bundle_path.join(old));
            }
            if std::fs::rename(picture, bundle_path.join(target)).is_err() {
                return;
            }
        } else if !is_custom {
            return;
        }
        let _ = bundle::refresh(&b);
        set_file_icon(&b);
        if let Some(folder) = Self::folder_of(bundle_path) {
            let _ = bundle::sync_exports(folder);
        }
    }

    async fn departed(self: Rc<Self>, folder: Folder, path: PathBuf, entry: Entry) {
        if entry.locked {
            return;
        }
        // The Trash writes its .trashinfo before it moves the file, but give
        // a slow filesystem a moment.
        sleep(150).await;
        let Some(trashed) = trash::find(&path) else {
            self.left_without_trash(folder, &path, &entry);
            return;
        };
        if is_running(&entry, &path, &trashed.file) {
            self.put_back(&trashed, &path);
            self.prompt.tell(
                &format!("“{}” is open", entry.name),
                &format!("Quit {}, then move it to the Trash.", entry.name),
            );
            return;
        }
        let everyone = if folder == Folder::Shared {
            " It is removed for every account on this computer."
        } else {
            ""
        };
        let body = match entry.kind {
            Kind::Flatpak => format!(
                "{} is in the Trash. Uninstalling removes the application.{everyone} Its settings and data are kept, so installing it again later picks up where you left off.",
                entry.name
            ),
            _ => format!(
                "{} is in the Trash. Uninstalling removes it from the Dock and Lens.{everyone} Empty the Trash to delete the file.",
                entry.name
            ),
        };
        let answer = self
            .prompt
            .ask(
                &format!("Uninstall “{}”?", entry.name),
                &body,
                &[("Keep", Style::Plain), ("Uninstall", Style::Destructive)],
            )
            .await;
        if answer != Some(1) {
            self.put_back(&trashed, &path);
            return;
        }
        if is_running(&entry, &path, &trashed.file) {
            self.put_back(&trashed, &path);
            self.prompt.tell(
                &format!("“{}” is open", entry.name),
                &format!("Quit {}, then move it to the Trash.", entry.name),
            );
            return;
        }
        match entry.kind {
            Kind::Flatpak => {
                self.busy.borrow_mut().insert(entry.id.clone());
                let (id, scope) = (entry.id.clone(), entry.scope);
                let result = blocking(move || flatpak::uninstall(&id, scope)).await;
                self.busy.borrow_mut().remove(&entry.id);
                if let Err(e) = result {
                    self.put_back(&trashed, &path);
                    self.prompt
                        .tell(&format!("{} could not be uninstalled", entry.name), &e);
                }
            }
            _ => {
                let _ = bundle::sync_exports(folder);
            }
        }
    }

    fn put_back(&self, trashed: &trash::Trashed, path: &Path) {
        if let Err(e) = trash::restore(trashed, path) {
            self.prompt
                .tell("The application could not be put back", &e);
        }
    }

    /// Shift+Delete, `rm`, or a drag into another folder. A Flatpak is still
    /// installed, so its entry comes back; only the Trash uninstalls.
    fn left_without_trash(self: &Rc<Self>, folder: Folder, path: &Path, entry: &Entry) {
        match entry.kind {
            Kind::Flatpak => {
                if folder == Folder::Home {
                    self.queue_mirror();
                }
                self.prompt.tell(
                    &format!("“{}” is still installed", entry.name),
                    "To uninstall it, move it to the Trash. To keep it in the Dock, drag it there.",
                );
            }
            Kind::Appimage => {
                let _ = bundle::sync_exports(folder);
                if folder == Folder::Home {
                    self.prompt.tell(
                        &format!("“{}” left Applications", entry.name),
                        &format!(
                            "{} is no longer in the Dock or Lens. Put it back in Applications to use it again.",
                            display_name(path)
                        ),
                    );
                }
            }
            Kind::System => {}
        }
    }

    fn mirror(self: &Rc<Self>) {
        let this = self.clone();
        let busy = self.busy.borrow().clone();
        glib::spawn_future_local(async move {
            let dir = Folder::Home.dir();
            let result = blocking(move || {
                reconcile::mirror(Folder::Home, |id| trash::holds_app(&dir, id, false), &busy)
            })
            .await;
            let Ok(changes) = result else { return };
            for path in changes.removed {
                this.known.borrow_mut().remove(&path);
                this.bundle_monitors.borrow_mut().remove(&path);
            }
            for path in changes.created {
                if let Some(b) = Bundle::read(&path) {
                    this.after_install(Folder::Home, &b);
                }
            }
        });
    }
}

/// Waits until a file that is still being copied in stops changing. False
/// when it went away meanwhile.
async fn wait_settled(path: &Path) -> bool {
    let stamp = |p: &Path| {
        std::fs::symlink_metadata(p)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    let mut last = stamp(path);
    loop {
        sleep(1000).await;
        let now = stamp(path);
        if now.is_none() {
            return false;
        }
        if now == last {
            return true;
        }
        last = now;
    }
}

fn is_running(entry: &Entry, original: &Path, trashed: &Path) -> bool {
    running::is_running(entry, original) || running::is_running(entry, trashed)
}

/// The entry's icon in Files, through GVFS metadata, which is kept per
/// account; each account's service sets it for both folders.
fn set_file_icon(b: &Bundle) {
    let (set_key, value, unset_key) = match bundle::icon(b) {
        icon::Icon::File(p) => (
            "metadata::custom-icon",
            gio::File::for_path(p).uri().to_string(),
            "metadata::custom-icon-name",
        ),
        icon::Icon::Name(n) => ("metadata::custom-icon-name", n, "metadata::custom-icon"),
    };
    let target = b.path.to_string_lossy().into_owned();
    let current = gio::File::for_path(&b.path)
        .query_info(
            "metadata::custom-icon,metadata::custom-icon-name",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok();
    if let Some(info) = &current {
        let has = |key: &str| info.attribute_string(key).map(|s| s.to_string());
        if has(set_key).as_deref() == Some(value.as_str()) && has(unset_key).is_none() {
            return;
        }
    }
    let _ = std::process::Command::new("gio")
        .args(["set", "-t", "string", &target, set_key, &value])
        .status();
    let _ = std::process::Command::new("gio")
        .args(["set", "-t", "unset", &target, unset_key])
        .status();
    // Files does not watch metadata another process writes. A new
    // modification time makes its monitor read the entry again.
    if let Ok(c) = std::ffi::CString::new(target) {
        // SAFETY: a null times pointer sets both times to now.
        unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), std::ptr::null(), 0) };
    }
}

/// Adds the shared Applications folder, Desktop, Documents, Downloads, and
/// Pictures to the Files sidebar, each once. A folder that does not exist yet
/// is added when it does. Removing one afterwards is the person's choice and
/// is not undone.
fn seed_bookmarks() {
    let record = paths::state_home().join("elavo-applications/bookmarks-seeded");
    let seeded = std::fs::read_to_string(&record).unwrap_or_default();
    let mut seeded: Vec<String> = seeded.lines().map(str::to_owned).collect();

    let home = glib::home_dir();
    let mut wanted = vec![(Folder::Shared.dir(), Some("Applications"))];
    for dir in [
        glib::UserDirectory::Desktop,
        glib::UserDirectory::Documents,
        glib::UserDirectory::Downloads,
        glib::UserDirectory::Pictures,
    ] {
        if let Some(path) = glib::user_special_dir(dir).filter(|p| *p != home) {
            wanted.push((path, None));
        }
    }

    let file = paths::config_home().join("gtk-3.0/bookmarks");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut changed = false;
    // Applications goes first, the way the Finder lists it.
    let mut at = 0;
    for (path, label) in wanted {
        let uri = gio::File::for_path(&path).uri().to_string();
        if seeded.contains(&uri) || !path.is_dir() {
            continue;
        }
        let present = lines
            .iter()
            .any(|l| l.split_whitespace().next() == Some(uri.as_str()));
        if !present {
            let line = match label {
                Some(label) => format!("{uri} {label}"),
                None => uri.clone(),
            };
            if label.is_some() {
                lines.insert(at, line);
                at += 1;
            } else {
                lines.push(line);
            }
            changed = true;
        }
        seeded.push(uri);
    }
    if changed {
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(&file, lines.join("\n") + "\n");
    }
    let _ = std::fs::create_dir_all(record.parent().unwrap());
    let _ = std::fs::write(&record, seeded.join("\n") + "\n");
}
