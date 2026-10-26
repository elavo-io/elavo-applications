//! `entry.json`, the record that says what a bundle is.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "entry.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Flatpak,
    Appimage,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    User,
    System,
}

impl Scope {
    pub fn flag(self) -> &'static str {
        match self {
            Scope::User => "--user",
            Scope::System => "--system",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    /// Flatpak application id, the system desktop file id without
    /// `.desktop`, or the id given to an AppImage.
    pub id: String,
    pub name: String,
    pub scope: Scope,
    /// The Flatpak remote, so a restored bundle can be reinstalled.
    #[serde(default)]
    pub origin: String,
    /// File name of the AppImage inside the bundle.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub locked: bool,
}

impl Entry {
    pub fn read(bundle: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(bundle.join(FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write(&self, bundle: &Path) -> crate::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())? + "\n";
        crate::bundle::write_file(&bundle.join(FILE), text.as_bytes(), 0o644)
    }

    /// The desktop id the dock, Lens, and window matching use. A Flatpak's
    /// own export, the system desktop file, or our AppImage export.
    pub fn desktop_id(&self) -> String {
        match self.kind {
            Kind::Flatpak | Kind::System => format!("{}.desktop", self.id),
            Kind::Appimage => format!("elavo-appimage-{}.desktop", self.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let entry = Entry {
            kind: Kind::Flatpak,
            id: "org.mozilla.firefox".into(),
            name: "Firefox".into(),
            scope: Scope::System,
            origin: "flathub".into(),
            source: String::new(),
            locked: false,
        };
        let text = serde_json::to_string(&entry).unwrap();
        assert!(text.contains("\"kind\":\"flatpak\""));
        assert!(text.contains("\"scope\":\"system\""));
        assert_eq!(serde_json::from_str::<Entry>(&text).unwrap(), entry);
        assert_eq!(entry.desktop_id(), "org.mozilla.firefox.desktop");
    }

    #[test]
    fn missing_optional_fields() {
        let e: Entry = serde_json::from_str(
            r#"{"kind":"appimage","id":"krita","name":"Krita","scope":"user"}"#,
        )
        .unwrap();
        assert!(!e.locked);
        assert_eq!(e.desktop_id(), "elavo-appimage-krita.desktop");
    }
}
