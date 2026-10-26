//! The Applications folder format and the operations on it, without GTK, so
//! the services, `elavo-applications-ctl`, and the root helper share it.
//!
//! An application is a `*.eapp` directory in `/Applications` (shared) or
//! `~/Applications` (this account). See `planning/applications/` in the
//! planning repository for the behaviour.

pub mod appimage;
pub mod bundle;
pub mod entry;
pub mod flatpak;
pub mod icon;
pub mod keyfile;
pub mod paths;
pub mod reconcile;
pub mod running;
pub mod system;
pub mod trash;

pub use bundle::Bundle;
pub use entry::{Entry, Kind, Scope};
pub use paths::Folder;

pub type Result<T> = std::result::Result<T, String>;

/// A file-name-safe, lower-case id: letters, digits, dots, and dashes.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') || out.ends_with('.') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

/// A name for a directory entry: no slashes, no leading dot.
pub fn file_name_safe(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c == '/' || c.is_control() { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        "Application".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("GitHub Desktop"), "github-desktop");
        assert_eq!(slug("  krita_5.2  "), "krita-5.2");
        assert_eq!(slug("!!!"), "app");
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name_safe("AC/DC"), "AC-DC");
        assert_eq!(file_name_safe(".hidden"), "hidden");
        assert_eq!(file_name_safe(""), "Application");
    }
}
