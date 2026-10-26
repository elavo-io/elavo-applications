//! Desktop files: enough of the freedesktop key file format to read an
//! application's `[Desktop Entry]` and to write ours.

use std::path::Path;

/// The keys of one group, in file order. Localized keys (`Name[de]`) are
/// kept as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    pub keys: Vec<(String, String)>,
}

impl Group {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        match self.keys.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.to_owned(),
            None => self.keys.push((key.to_owned(), value.to_owned())),
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.keys.retain(|(k, _)| k != key);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyFile {
    pub groups: Vec<(String, Group)>,
}

impl KeyFile {
    pub fn parse(text: &str) -> Self {
        let mut file = KeyFile::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                file.groups.push((name.to_owned(), Group::default()));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if let Some((_, group)) = file.groups.last_mut() {
                group
                    .keys
                    .push((key.trim().to_owned(), value.trim().to_owned()));
            }
        }
        file
    }

    pub fn read(path: &Path) -> Option<Self> {
        std::fs::read_to_string(path).ok().map(|t| Self::parse(&t))
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|(n, _)| n == name).map(|(_, g)| g)
    }

    pub fn group_mut(&mut self, name: &str) -> &mut Group {
        if let Some(i) = self.groups.iter().position(|(n, _)| n == name) {
            return &mut self.groups[i].1;
        }
        self.groups.push((name.to_owned(), Group::default()));
        &mut self.groups.last_mut().unwrap().1
    }

    pub fn entry(&self) -> Option<&Group> {
        self.group("Desktop Entry")
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, (name, group)) in self.groups.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("[{name}]\n"));
            for (k, v) in &group.keys {
                out.push_str(&format!("{k}={v}\n"));
            }
        }
        out
    }
}

/// One argument of an `Exec` line, quoted when it needs to be.
pub fn exec_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@".contains(c));
    if plain {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_groups_and_order() {
        let text = "# c\n[Desktop Entry]\nName=Krita\nName[de]=Krita DE\nExec=krita %F\n\n[Desktop Action new]\nName=New\nExec=krita --new\n";
        let file = KeyFile::parse(text);
        assert_eq!(file.entry().unwrap().get("Name"), Some("Krita"));
        assert_eq!(file.entry().unwrap().get("Name[de]"), Some("Krita DE"));
        assert_eq!(file.groups.len(), 2);
        assert_eq!(KeyFile::parse(&file.render()), file);
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(
            exec_arg("/Applications/Krita.eapp/run"),
            "/Applications/Krita.eapp/run"
        );
        assert_eq!(exec_arg("/a b/run"), "\"/a b/run\"");
        assert_eq!(exec_arg("100%$"), "\"100%%\\$\"");
    }
}
