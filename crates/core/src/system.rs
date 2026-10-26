//! System applications: RPMs in the image that appear in `/Applications` as
//! locked entries. The image lists which ones, one desktop id per line.

use std::path::PathBuf;

pub fn allowlist_path() -> PathBuf {
    std::env::var_os("ELAVO_SYSTEM_APPLICATIONS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/elavo-applications/system-applications"))
}

pub fn parse_allowlist(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.strip_suffix(".desktop").unwrap_or(l).to_owned())
        .collect()
}

pub fn allowlist() -> Vec<String> {
    std::fs::read_to_string(allowlist_path())
        .map(|t| parse_allowlist(&t))
        .unwrap_or_default()
}

pub fn desktop_file(id: &str) -> Option<PathBuf> {
    ["/usr/local/share/applications", "/usr/share/applications"]
        .iter()
        .map(|d| PathBuf::from(d).join(format!("{id}.desktop")))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_lines() {
        let got = parse_allowlist(
            "# Files\norg.gnome.Nautilus.desktop\n\nio.elavo.SystemSettings # Settings\n",
        );
        assert_eq!(got, ["org.gnome.Nautilus", "io.elavo.SystemSettings"]);
    }
}
