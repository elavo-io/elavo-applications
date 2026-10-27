//! `~/.config/elavo/applications.conf`, which System Settings also edits.
//!
//! ```ini
//! [Install]
//! AfterInstall=ask
//! ```

use std::path::PathBuf;

use crate::keyfile::KeyFile;
use crate::paths;

const GROUP: &str = "Install";
const KEY: &str = "AfterInstall";

/// What happens once something the person installed is ready.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AfterInstall {
    /// Ask whether to open it, with a box to remember the answer.
    #[default]
    Ask,
    Open,
    Nothing,
}

impl AfterInstall {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Open => "open",
            Self::Nothing => "nothing",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Self::Open,
            "nothing" => Self::Nothing,
            _ => Self::Ask,
        }
    }
}

pub fn path() -> PathBuf {
    paths::config_home().join("elavo/applications.conf")
}

pub fn after_install() -> AfterInstall {
    KeyFile::read(&path())
        .and_then(|f| {
            f.group(GROUP)
                .and_then(|g| g.get(KEY))
                .map(AfterInstall::parse)
        })
        .unwrap_or_default()
}

pub fn set_after_install(value: AfterInstall) -> std::io::Result<()> {
    let path = path();
    let mut file = KeyFile::read(&path).unwrap_or_default();
    file.group_mut(GROUP).set(KEY, value.as_str());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, file.render())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(AfterInstall::parse("open"), AfterInstall::Open);
        assert_eq!(AfterInstall::parse(" Nothing"), AfterInstall::Nothing);
        assert_eq!(AfterInstall::parse("ask"), AfterInstall::Ask);
        assert_eq!(AfterInstall::parse("sometimes"), AfterInstall::Ask);
        for v in [AfterInstall::Ask, AfterInstall::Open, AfterInstall::Nothing] {
            assert_eq!(AfterInstall::parse(v.as_str()), v);
        }
    }

    #[test]
    fn keeps_other_keys() {
        let mut file = KeyFile::parse("[Install]\nOther=1\n\n[Else]\nA=b\n");
        file.group_mut(GROUP).set(KEY, "open");
        let text = file.render();
        assert!(text.contains("Other=1"));
        assert!(text.contains("A=b"));
        assert_eq!(
            KeyFile::parse(&text).group(GROUP).and_then(|g| g.get(KEY)),
            Some("open")
        );
    }
}
