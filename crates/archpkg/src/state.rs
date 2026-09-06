//! The small state the GUI and the tray applet share: the last check's pending updates and
//! when it ran. Lives in `$XDG_STATE_HOME/fuide-arch-update/` (`FUIDE_ARCH_STATE_DIR`
//! overrides it). Writes are atomic (temp file + rename) so a reader never sees a torn file.

use std::path::{Path, PathBuf};

use crate::pacman::Upgrade;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckState {
    pub updates: Vec<Upgrade>,
    /// Unix time of the check; `None` = never checked.
    pub checked_at: Option<u64>,
    /// The last check failed with this message (updates are the previous good list).
    pub error: Option<String>,
}

pub fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FUIDE_ARCH_STATE_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
                .join(".local/state")
        });
    base.join("fuide-arch-update")
}

pub fn state_file() -> PathBuf {
    state_dir().join("check")
}

/// Serialise: header lines `checked_at=<epoch>` / `error=<text>`, then one update per line
/// as `<repo|aur>\t<name>\t<current>\t<new>`.
pub fn to_text(s: &CheckState) -> String {
    let mut out = String::from("# fuide-arch-update check state\n");
    if let Some(t) = s.checked_at {
        out.push_str(&format!("checked_at={t}\n"));
    }
    if let Some(e) = &s.error {
        out.push_str(&format!("error={}\n", e.replace('\n', " ")));
    }
    for u in &s.updates {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            if u.aur { "aur" } else { "repo" },
            u.name,
            u.current,
            u.new
        ));
    }
    out
}

pub fn parse(text: &str) -> CheckState {
    let mut s = CheckState::default();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(v) = line.strip_prefix("checked_at=") {
            s.checked_at = v.trim().parse().ok();
            continue;
        }
        if let Some(v) = line.strip_prefix("error=") {
            s.error = Some(v.trim().to_string());
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() == 4 {
            s.updates.push(Upgrade {
                aur: f[0] == "aur",
                name: f[1].to_string(),
                current: f[2].to_string(),
                new: f[3].to_string(),
            });
        }
    }
    s
}

pub fn load() -> CheckState {
    load_from(&state_file())
}

pub fn load_from(path: &Path) -> CheckState {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

pub fn save(s: &CheckState) -> std::io::Result<()> {
    save_to(&state_file(), s)
}

pub fn save_to(path: &Path, s: &CheckState) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, to_text(s))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = CheckState {
            updates: vec![
                Upgrade {
                    name: "bash".into(),
                    current: "5.3-1".into(),
                    new: "5.3-2".into(),
                    aur: false,
                },
                Upgrade {
                    name: "yay".into(),
                    current: "13.0.0-1".into(),
                    new: "13.0.1-1".into(),
                    aur: true,
                },
            ],
            checked_at: Some(1_788_675_272),
            error: None,
        };
        assert_eq!(parse(&to_text(&s)), s);
        let e = CheckState {
            error: Some("network\ndown".into()),
            ..Default::default()
        };
        assert_eq!(parse(&to_text(&e)).error.as_deref(), Some("network down"));
        assert_eq!(parse("garbage\n\n"), CheckState::default());
    }

    #[test]
    fn save_and_load_create_the_directory() {
        let dir = std::env::temp_dir().join(format!("archpkg-state-{}", std::process::id()));
        let path = dir.join("nested/check");
        assert_eq!(load_from(&path), CheckState::default());
        let s = CheckState {
            checked_at: Some(1),
            ..Default::default()
        };
        save_to(&path, &s).unwrap();
        assert_eq!(load_from(&path), s);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
