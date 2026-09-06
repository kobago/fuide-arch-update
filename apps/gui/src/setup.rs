//! `fuide-arch-update --setup` / `--unsetup`: per-user desktop integration for a `cargo install`
//! (or any other) copy of the binaries, no root needed.
//!
//! `--setup` writes, with absolute `Exec=` paths derived from this executable:
//! - `$XDG_DATA_HOME/applications/fuide-arch-update{,-tray}.desktop` (start menu)
//! - `$XDG_DATA_HOME/icons/hicolor/scalable/apps/fuide-arch-update.svg` (icon)
//! - `$XDG_CONFIG_HOME/autostart/fuide-arch-update-tray.desktop` (tray at login)
//!
//! then starts the tray now. `--unsetup` removes those files (the tray keeps running until its
//! `Quit`). The polkit policy is system-wide only, so it is not handled here: without it every
//! root operation asks for the password (see README).

use std::path::{Path, PathBuf};

const GUI_DESKTOP: &str = include_str!("../../../res/fuide-arch-update.desktop");
const TRAY_DESKTOP: &str = include_str!("../../../res/fuide-arch-update-tray.desktop");
const ICON_SVG: &str = include_str!("../../../res/fuide-arch-update.svg");

/// Where the files go and what they contain. Pure: built from paths, written by `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub gui_desktop: (PathBuf, String),
    pub tray_desktop: (PathBuf, String),
    pub autostart: (PathBuf, String),
    pub icon: (PathBuf, &'static str),
    pub tray_bin: PathBuf,
    /// `$XDG_DATA_HOME`, for the menu-cache refresh.
    pub data_home: PathBuf,
}

impl Plan {
    /// `data_home` = `$XDG_DATA_HOME`, `config_home` = `$XDG_CONFIG_HOME`.
    pub fn new(gui_bin: &Path, tray_bin: &Path, data_home: &Path, config_home: &Path) -> Self {
        let apps = data_home.join("applications");
        let gui = with_exec(GUI_DESKTOP, &exec_quote(gui_bin));
        // The tray waits a moment for the panel's StatusNotifierWatcher to come up.
        let tray = with_exec(
            TRAY_DESKTOP,
            &format!("/bin/sh -c \"sleep 3 && exec {}\"", exec_quote(tray_bin)),
        );
        Self {
            gui_desktop: (apps.join("fuide-arch-update.desktop"), gui),
            tray_desktop: (apps.join("fuide-arch-update-tray.desktop"), tray.clone()),
            autostart: (
                config_home.join("autostart/fuide-arch-update-tray.desktop"),
                tray,
            ),
            icon: (
                data_home.join("icons/hicolor/scalable/apps/fuide-arch-update.svg"),
                ICON_SVG,
            ),
            tray_bin: tray_bin.to_path_buf(),
            data_home: data_home.to_path_buf(),
        }
    }

    /// From the environment: this executable, its sibling tray binary, XDG dirs.
    pub fn from_env() -> Result<Self, String> {
        let gui_bin = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("cannot locate this executable: {e}"))?;
        let tray_bin = gui_bin
            .parent()
            .map(|d| d.join("fuide-arch-update-tray"))
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                format!(
                    "fuide-arch-update-tray not found next to {}\n\
                     install both binaries, e.g. `cargo install --git <repo> fuide-arch-update fuide-arch-update-tray`",
                    gui_bin.display()
                )
            })?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let data_home = xdg_dir("XDG_DATA_HOME", &home.join(".local/share"));
        let config_home = xdg_dir("XDG_CONFIG_HOME", &home.join(".config"));
        Ok(Self::new(&gui_bin, &tray_bin, &data_home, &config_home))
    }

    pub fn files(&self) -> [(&Path, &str); 4] {
        [
            (&self.gui_desktop.0, &self.gui_desktop.1),
            (&self.tray_desktop.0, &self.tray_desktop.1),
            (&self.autostart.0, &self.autostart.1),
            (&self.icon.0, self.icon.1),
        ]
    }

    /// Write every file (creating directories). Returns the paths written.
    pub fn apply(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut done = Vec::new();
        for (path, body) in self.files() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, body)?;
            done.push(path.to_path_buf());
        }
        Ok(done)
    }

    /// Remove every file that exists. Returns the paths removed.
    pub fn remove(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut done = Vec::new();
        for (path, _) in self.files() {
            match std::fs::remove_file(path) {
                Ok(()) => done.push(path.to_path_buf()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }
}

fn xdg_dir(var: &str, default: &Path) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => default.to_path_buf(),
    }
}

/// Replace the `Exec=` line of a desktop entry.
fn with_exec(template: &str, exec: &str) -> String {
    let mut out = String::with_capacity(template.len() + exec.len());
    for line in template.lines() {
        if line.starts_with("Exec=") {
            out.push_str("Exec=");
            out.push_str(exec);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A path as one argument of a desktop-entry `Exec=` value: double-quoted when it needs it,
/// with the characters the spec reserves inside quotes backslash-escaped.
fn exec_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    let plain = s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+@:,".contains(&b));
    if plain {
        return s.into_owned();
    }
    let mut q = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    q
}

/// Run a helper quietly; a missing tool or a failure is not an error here.
fn quiet(program: &str, args: &[&std::ffi::OsStr]) {
    let _ = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Tell the desktop about the new / removed entries and icon. Without this KDE and GTK keep
/// serving their caches and the menu shows the entry without its icon until the next login:
/// - `update-desktop-database`: the menu cache
/// - `gtk-update-icon-cache`: a stale `icon-theme.cache` in the user's hicolor dir (KDE reads it
///   too) is rebuilt; none is created when there was none
/// - the `org.kde.KIconLoader.iconChanged` D-Bus signal: running KDE apps drop their icon caches
fn refresh_desktop(data_home: &Path) {
    quiet(
        "update-desktop-database",
        &[data_home.join("applications").as_os_str()],
    );
    let hicolor = data_home.join("icons/hicolor");
    if hicolor.join("icon-theme.cache").is_file() {
        quiet(
            "gtk-update-icon-cache",
            &[
                "-q".as_ref(),
                "-t".as_ref(),
                "-f".as_ref(),
                hicolor.as_os_str(),
            ],
        );
    }
    quiet(
        "dbus-send",
        &[
            "--session".as_ref(),
            "--type=signal".as_ref(),
            "/KIconLoader".as_ref(),
            "org.kde.KIconLoader.iconChanged".as_ref(),
            "int32:0".as_ref(),
        ],
    );
}

/// Start the tray now, detached. It exits by itself when one is already running.
fn start_tray(tray_bin: &Path) -> std::io::Result<()> {
    std::process::Command::new(tray_bin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// `--setup`: exit code for `main`.
pub fn setup() -> i32 {
    let plan = match Plan::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fuide-arch-update --setup: {e}");
            return 1;
        }
    };
    match plan.apply() {
        Ok(paths) => {
            for p in paths {
                println!("wrote  {}", p.display());
            }
        }
        Err(e) => {
            eprintln!("fuide-arch-update --setup: {e}");
            return 1;
        }
    }
    refresh_desktop(&plan.data_home);
    match start_tray(&plan.tray_bin) {
        Ok(()) => println!("started {}", plan.tray_bin.display()),
        Err(e) => eprintln!("could not start the tray now ({e}); it starts at the next login"),
    }
    println!(
        "\nDone: start-menu entries, icon, and the tray at login (~/.config/autostart).\n\
         If the menu shows no icon yet, log out and in once.\n\
         Optional, root: keep the polkit authorisation for a few minutes instead of asking every time:\n\
         \x20 sudo install -Dm644 res/org.kobago.fuide-arch-update.policy /usr/share/polkit-1/actions/\n\
         Undo with `fuide-arch-update --unsetup`."
    );
    0
}

/// `--unsetup`: exit code for `main`.
pub fn unsetup() -> i32 {
    let plan = match Plan::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fuide-arch-update --unsetup: {e}");
            return 1;
        }
    };
    match plan.remove() {
        Ok(paths) if paths.is_empty() => println!("nothing to remove"),
        Ok(paths) => {
            for p in paths {
                println!("removed {}", p.display());
            }
            refresh_desktop(&plan.data_home);
            println!("The running tray (if any) keeps going until its Quit.");
        }
        Err(e) => {
            eprintln!("fuide-arch-update --unsetup: {e}");
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan::new(
            Path::new("/home/u/.cargo/bin/fuide-arch-update"),
            Path::new("/home/u/.cargo/bin/fuide-arch-update-tray"),
            Path::new("/home/u/.local/share"),
            Path::new("/home/u/.config"),
        )
    }

    #[test]
    fn exec_lines_use_absolute_paths() {
        let p = plan();
        assert!(p
            .gui_desktop
            .1
            .contains("\nExec=/home/u/.cargo/bin/fuide-arch-update\n"));
        assert!(p.tray_desktop.1.contains(
            "\nExec=/bin/sh -c \"sleep 3 && exec /home/u/.cargo/bin/fuide-arch-update-tray\"\n"
        ));
        assert_eq!(p.tray_desktop.1, p.autostart.1);
        // Everything else from the templates survives.
        assert!(p.gui_desktop.1.contains("StartupWMClass=fuide-arch-update"));
        assert!(p.tray_desktop.1.contains("X-KDE-autostart-phase=2"));
        assert!(p.icon.1.contains("<svg"));
    }

    #[test]
    fn file_locations() {
        let p = plan();
        let paths: Vec<_> = p.files().iter().map(|(p, _)| p.to_path_buf()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("/home/u/.local/share/applications/fuide-arch-update.desktop"),
                PathBuf::from("/home/u/.local/share/applications/fuide-arch-update-tray.desktop"),
                PathBuf::from("/home/u/.config/autostart/fuide-arch-update-tray.desktop"),
                PathBuf::from(
                    "/home/u/.local/share/icons/hicolor/scalable/apps/fuide-arch-update.svg"
                ),
            ]
        );
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(
            exec_quote(Path::new("/opt/a-b_c.1/bin")),
            "/opt/a-b_c.1/bin"
        );
        assert_eq!(
            exec_quote(Path::new("/home/my user/bin/x")),
            "\"/home/my user/bin/x\""
        );
        assert_eq!(exec_quote(Path::new("/a$b\"c")), "\"/a\\$b\\\"c\"");
    }

    #[test]
    fn apply_then_remove_round_trip() {
        let tmp = std::env::temp_dir().join(format!("fau-setup-{}", std::process::id()));
        let p = Plan::new(
            Path::new("/x/fuide-arch-update"),
            Path::new("/x/fuide-arch-update-tray"),
            &tmp.join("share"),
            &tmp.join("config"),
        );
        let written = p.apply().unwrap();
        assert_eq!(written.len(), 4);
        for (path, body) in p.files() {
            assert_eq!(std::fs::read_to_string(path).unwrap(), body);
        }
        let removed = p.remove().unwrap();
        assert_eq!(removed, written);
        assert!(p.remove().unwrap().is_empty(), "second remove is a no-op");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
