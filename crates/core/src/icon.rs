//! Which icon a bundle shows: the person's own file, then the Elavo theme,
//! then the standardized icon (the application's icon on Elavo's template),
//! then the application's own icon, then the generic glyph.

use std::path::{Path, PathBuf};

use gdk_pixbuf::{glib, Colorspace, InterpType, Pixbuf};

pub const CUSTOM: [&str; 3] = ["custom-icon.png", "custom-icon.svg", "custom-icon.webp"];
pub const BUNDLED: [&str; 2] = ["icon.png", "icon.svg"];
pub const STANDARD: &str = "standard-icon.png";
pub const FALLBACK: &str = "application-x-executable";
const THEME: &str = "Elavo";
/// The standardized icon's size. Files' largest zoom on a 2x screen.
const SIZE: i32 = 512;
/// The width of the application's icon on a 1024 pixel template.
const ART_WIDTH: i32 = 595;
const TEMPLATE_DIR: &str = "/usr/share/elavo-applications/standardized-icons";

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
    if let Some(name) = theme_names(id, own).into_iter().find(|n| theme_has(n)) {
        return Icon::Name(name);
    }
    if let Some(path) = standard(bundle, own) {
        return Icon::File(path);
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

/// Names to look for in the Elavo theme, in order. The application's id,
/// then the icon its own desktop file names, then the older GNOME name
/// (`org.gnome.Aisleriot` is drawn as `gnome-aisleriot`).
fn theme_names(id: &str, own: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |name: &str| {
        if !name.is_empty() && !name.contains('/') && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    };
    push(id);
    if let Some(own) = own {
        push(own);
    }
    if let Some(rest) = id.strip_prefix("org.gnome.") {
        push(&format!("gnome-{}", rest.to_ascii_lowercase()));
    }
    names
}

fn template_dir() -> Option<PathBuf> {
    let mut dirs = vec![PathBuf::from(TEMPLATE_DIR)];
    if cfg!(debug_assertions) {
        dirs.push(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_resources/standardized-icons"),
        );
    }
    dirs.into_iter()
        .find(|d| d.join("template.png").is_file() && d.join("mask.png").is_file())
}

/// The application's own icon as a file: the one copied into the bundle, or
/// its `Icon=` looked up in hicolor, largest first.
fn source(bundle: &Path, own: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = bundled(bundle) {
        return Some(path);
    }
    let name = own.filter(|n| !n.is_empty())?;
    if name.starts_with('/') {
        return Some(PathBuf::from(name)).filter(|p| p.is_file());
    }
    if name.contains('/') {
        return None;
    }
    let data_home = crate::paths::data_home();
    let mut roots = vec![data_home.join("flatpak/exports/share"), data_home];
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    roots.extend(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    roots.extend(
        [
            "/var/lib/flatpak/exports/share",
            "/usr/local/share",
            "/usr/share",
        ]
        .map(PathBuf::from),
    );
    let sizes = [
        "scalable",
        "1024x1024",
        "512x512",
        "256x256",
        "192x192",
        "128x128",
        "96x96",
        "64x64",
        "48x48",
    ];
    sizes.iter().find_map(|size| {
        roots.iter().find_map(|root| {
            ["svg", "png"].iter().find_map(|ext| {
                let path = root
                    .join("icons/hicolor")
                    .join(size)
                    .join("apps")
                    .join(format!("{name}.{ext}"));
                path.is_file().then_some(path)
            })
        })
    })
}

/// `standard-icon.png` in the bundle, made again when the application's icon
/// or the template is newer. None when there is no icon to put on it.
fn standard(bundle: &Path, own: Option<&str>) -> Option<PathBuf> {
    let src = source(bundle, own)?;
    let target = bundle.join(STANDARD);
    let existing = || target.is_file().then(|| target.clone());
    let Some(templates) = template_dir() else {
        return existing();
    };
    let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let newest = [
        stamp(&src),
        stamp(&templates.join("template.png")),
        stamp(&templates.join("mask.png")),
    ]
    .into_iter()
    .flatten()
    .max();
    let made = stamp(&target);
    if made.is_some() && made >= newest && png_size(&target) == Some((SIZE, SIZE)) {
        return Some(target);
    }
    let Ok(pixbuf) = standardize(&src, &templates) else {
        return existing();
    };
    let part = bundle.join(format!(".{STANDARD}.part"));
    if pixbuf.savev(&part, "png", &[]).is_err() || std::fs::rename(&part, &target).is_err() {
        let _ = std::fs::remove_file(&part);
        return existing();
    }
    Some(target)
}

/// Width and height from a PNG's header, without decoding it.
fn png_size(path: &Path) -> Option<(i32, i32)> {
    use std::io::Read;
    let mut head = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let field = |at: usize| i32::from_be_bytes(head[at..at + 4].try_into().unwrap());
    Some((field(16), field(20)))
}

/// Puts `icon` on the template in `templates`, at 512 pixels: scaled to 595
/// of every 1024 pixels wide, centered, and cut to the mask where a tall icon
/// runs past the template's shape. The template and mask are drawn at 1024
/// and scaled down here.
pub fn standardize(icon: &Path, templates: &Path) -> Result<Pixbuf, String> {
    let err = |e: glib::Error| e.to_string();
    let load = |name: &str| -> Result<Pixbuf, String> {
        let image = with_alpha(Pixbuf::from_file(templates.join(name)).map_err(err)?)?;
        if (image.width(), image.height()) == (SIZE, SIZE) {
            return Ok(image);
        }
        image
            .scale_simple(SIZE, SIZE, InterpType::Hyper)
            .ok_or_else(|| format!("{name} could not be scaled"))
    };
    let template = load("template.png")?;
    let mask = load("mask.png")?;
    let art_width = (ART_WIDTH * SIZE + 512) / 1024;
    let art = with_alpha(Pixbuf::from_file_at_scale(icon, art_width, -1, true).map_err(err)?)?;
    Ok(compose(&template, &mask, &art))
}

fn with_alpha(pixbuf: Pixbuf) -> Result<Pixbuf, String> {
    if pixbuf.has_alpha() && pixbuf.n_channels() == 4 && pixbuf.bits_per_sample() == 8 {
        return Ok(pixbuf);
    }
    pixbuf.add_alpha(false, 0, 0, 0).map_err(|e| e.to_string())
}

/// `art` centered over `template`, its alpha multiplied by the mask's.
fn compose(template: &Pixbuf, mask: &Pixbuf, art: &Pixbuf) -> Pixbuf {
    let (width, height) = (template.width(), template.height());
    let (stride, mask_stride, art_stride) = (
        template.rowstride() as usize,
        mask.rowstride() as usize,
        art.rowstride() as usize,
    );
    let mut out = template.read_pixel_bytes().to_vec();
    let mask_px = mask.read_pixel_bytes();
    let art_px = art.read_pixel_bytes();
    let (left, top) = ((width - art.width()) / 2, (height - art.height()) / 2);
    for y in 0..art.height() {
        let cy = top + y;
        if !(0..height).contains(&cy) {
            continue;
        }
        for x in 0..art.width() {
            let cx = left + x;
            if !(0..width).contains(&cx) {
                continue;
            }
            let a_at = y as usize * art_stride + x as usize * 4;
            let m = mask_px[cy as usize * mask_stride + cx as usize * 4 + 3] as u32;
            let a = art_px[a_at + 3] as u32 * m / 255;
            if a == 0 {
                continue;
            }
            let t_at = cy as usize * stride + cx as usize * 4;
            let ta = out[t_at + 3] as u32;
            let under = ta * (255 - a) / 255;
            let oa = a + under;
            for c in 0..3 {
                let value = art_px[a_at + c] as u32 * a + out[t_at + c] as u32 * under;
                out[t_at + c] = (value / oa) as u8;
            }
            out[t_at + 3] = oa as u8;
        }
    }
    Pixbuf::from_bytes(
        &glib::Bytes::from_owned(out),
        Colorspace::Rgb,
        true,
        8,
        width,
        height,
        stride as i32,
    )
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

    #[test]
    fn gnome_apps_keep_their_older_icon_name() {
        assert_eq!(
            theme_names("org.gnome.Aisleriot", Some("org.gnome.Aisleriot")),
            ["org.gnome.Aisleriot", "gnome-aisleriot"]
        );
        assert_eq!(
            theme_names("com.rtsoft.patchy", Some("com.rtsoft.patchy")),
            ["com.rtsoft.patchy"]
        );
    }

    #[test]
    fn a_tall_icon_is_centered_and_cut_to_the_mask() {
        let templates = template_dir().expect("the templates in _resources");
        let dir = std::env::temp_dir().join(format!("eapp-standard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tall = Pixbuf::new(Colorspace::Rgb, true, 8, 100, 400).unwrap();
        tall.fill(0xff0000ff);
        let icon = dir.join("icon.png");
        tall.savev(&icon, "png", &[]).unwrap();

        let out = standardize(&icon, &templates).unwrap();
        assert_eq!((out.width(), out.height()), (512, 512));
        let px = out.read_pixel_bytes();
        let at = |x: usize, y: usize| {
            let i = y * out.rowstride() as usize + x * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(256, 256), [255, 0, 0, 255]);
        // Scaled to 298 wide the icon is 1192 tall; above the mask it is cut.
        assert_eq!(at(256, 20)[3], 0);
        // Outside the icon's 298 columns the template shows.
        assert_ne!(at(75, 256), [255, 0, 0, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
