//! Worker threads around `archpkg`: inventory, update checks, searches and detail lookups run
//! off the UI thread and come back as [`Msg`]. Mutating commands (`pkexec pacman …`, the AUR
//! helper) run here too, non-interactively, streaming their output line by line into the
//! event log; root authentication is polkit's job (the desktop shows its own dialog).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use archpkg::pacman::{self, Package, SearchHit, Upgrade};
use archpkg::state::{self, CheckState};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemInfo {
    pub kernel: String,
    pub reboot_required: bool,
    pub cache_kib: Option<f64>,
    pub cache_candidates: Option<u32>,
    pub last_sync: Option<u64>,
    pub aur_helper: Option<String>,
    /// `pkexec` (or the test override); `None` = no polkit, root commands are refused.
    pub privilege: Option<String>,
    pub checkupdates: bool,
    pub tray_running: bool,
}

/// A command that changes the system: what to run and how to label it in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
}

impl Job {
    pub fn command_line(&self) -> String {
        let prog = std::path::Path::new(&self.program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone());
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                std::path::Path::new(a)
                    .file_name()
                    .filter(|_| a.starts_with('/'))
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| a.clone())
            })
            .collect();
        format!("{prog} {}", args.join(" ")).trim_end().to_string()
    }
}

pub enum Msg {
    /// `pacman -Qi` (+ foreign / orphan marks). Elapsed ms.
    Inventory(Result<Vec<Package>, String>, f32),
    /// `checkupdates` + `<helper> -Qua`; the state file has been rewritten.
    Check(Result<Vec<Upgrade>, String>),
    Search {
        query: String,
        result: Result<Vec<SearchHit>, String>,
    },
    /// `-Si` details for packages that are not installed (search results).
    Details(Result<Vec<Package>, String>),
    System(SystemInfo),
    /// One line of output from the running job.
    Line {
        text: String,
        stderr: bool,
    },
    /// The running job finished.
    Exit {
        label: String,
        ok: bool,
        code: Option<i32>,
        elapsed_secs: f32,
    },
}

pub struct Backend {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    fetching: bool,
    checking: bool,
    searching: Option<String>,
    details_pending: usize,
    system_pending: usize,
    running: Option<Job>,
}

impl Default for Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            fetching: false,
            checking: false,
            searching: None,
            details_pending: 0,
            system_pending: 0,
            running: None,
        }
    }

    pub fn fetching(&self) -> bool {
        self.fetching
    }
    pub fn checking(&self) -> bool {
        self.checking
    }
    pub fn searching(&self) -> Option<&str> {
        self.searching.as_deref()
    }
    pub fn running(&self) -> Option<&Job> {
        self.running.as_ref()
    }
    /// Anything in flight (tests wait on this).
    #[allow(dead_code)]
    pub fn busy(&self) -> bool {
        self.fetching
            || self.checking
            || self.searching.is_some()
            || self.details_pending > 0
            || self.system_pending > 0
            || self.running.is_some()
    }

    #[cfg(test)]
    pub fn inject(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    pub fn poll(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            match &m {
                Msg::Inventory(..) => self.fetching = false,
                Msg::Check(_) => self.checking = false,
                Msg::Search { .. } => self.searching = None,
                Msg::Details(_) => self.details_pending = self.details_pending.saturating_sub(1),
                Msg::System(_) => self.system_pending = self.system_pending.saturating_sub(1),
                Msg::Line { .. } => {}
                Msg::Exit { .. } => self.running = None,
            }
            out.push(m);
        }
        out
    }

    /// Read the installed packages (a few hundred ms).
    pub fn fetch_inventory(&mut self, ctx: egui::Context) {
        if self.fetching {
            return;
        }
        self.fetching = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let result = pacman::installed();
            let _ = tx.send(Msg::Inventory(result, t0.elapsed().as_secs_f32() * 1000.0));
            ctx.request_repaint();
        });
    }

    /// Repo + AUR update check (network). The result is written to the shared state file so
    /// the tray applet picks it up.
    pub fn check(&mut self, ctx: egui::Context) {
        if self.checking {
            return;
        }
        self.checking = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = pacman::check_all();
            let mut st = state::load();
            st.checked_at = Some(archpkg::now_epoch());
            match &result {
                Ok(u) => {
                    st.updates = u.clone();
                    st.error = None;
                }
                Err(e) => st.error = Some(e.clone()),
            }
            let _ = state::save(&st);
            let _ = tx.send(Msg::Check(result));
            ctx.request_repaint();
        });
    }

    /// `pacman -Ss` + `<helper> -Ss --aur`.
    pub fn search(&mut self, query: String, ctx: egui::Context) {
        self.searching = Some(query.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<SearchHit>, String> {
                let mut hits = pacman::search_repo(&query)?;
                match pacman::search_aur(&query) {
                    Ok(aur) => hits.extend(aur),
                    // AUR RPC down: keep the repo hits
                    Err(e) => {
                        if hits.is_empty() {
                            return Err(e);
                        }
                    }
                }
                Ok(hits)
            })();
            let _ = tx.send(Msg::Search { query, result });
            ctx.request_repaint();
        });
    }

    /// Details for not-installed packages: `repo_names` via `pacman -Si`, `aur_names` via the
    /// helper (network).
    pub fn details(&mut self, repo_names: Vec<String>, aur_names: Vec<String>, ctx: egui::Context) {
        if repo_names.is_empty() && aur_names.is_empty() {
            return;
        }
        self.details_pending += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<Package>, String> {
                let mut v = pacman::info_repo(&repo_names)?;
                v.extend(pacman::info_aur(&aur_names)?);
                Ok(v)
            })();
            let _ = tx.send(Msg::Details(result));
            ctx.request_repaint();
        });
    }

    /// Kernel / cache / sync time / helpers (cheap, no network).
    pub fn fetch_system(&mut self, ctx: egui::Context) {
        self.system_pending += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (kernel, reboot_required) = pacman::kernel();
            let info = SystemInfo {
                kernel,
                reboot_required,
                cache_kib: pacman::cache_size_kib(),
                cache_candidates: pacman::cache_candidates(2),
                last_sync: pacman::last_sync_epoch(),
                aur_helper: pacman::aur_helper().map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string())
                }),
                privilege: pacman::privilege_cmd(),
                checkupdates: pacman::has_checkupdates(),
                tray_running: Command::new("pgrep")
                    .args(["-x", "fuide-arch-update-tray"])
                    .output()
                    .is_ok_and(|o| !o.stdout.is_empty()),
            };
            let _ = tx.send(Msg::System(info));
            ctx.request_repaint();
        });
    }

    /// Run a mutating command, streaming its output. Returns false if one is already running.
    /// Non-interactive: stdin is `/dev/null`, `LC_ALL=C.UTF-8`, colours off.
    pub fn run(&mut self, job: Job, ctx: egui::Context) -> bool {
        if self.running.is_some() {
            return false;
        }
        self.running = Some(job.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let child = Command::new(&job.program)
                .args(&job.args)
                .env("LC_ALL", "C.UTF-8")
                .env_remove("LANGUAGE")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Msg::Line {
                        text: format!("cannot start {}: {e}", job.program),
                        stderr: true,
                    });
                    let _ = tx.send(Msg::Exit {
                        label: job.label,
                        ok: false,
                        code: None,
                        elapsed_secs: 0.0,
                    });
                    ctx.request_repaint();
                    return;
                }
            };
            let mut readers = Vec::new();
            for (stream, is_err) in [
                (
                    child
                        .stdout
                        .take()
                        .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
                    false,
                ),
                (
                    child
                        .stderr
                        .take()
                        .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
                    true,
                ),
            ] {
                let Some(stream) = stream else { continue };
                let tx = tx.clone();
                let ctx = ctx.clone();
                readers.push(std::thread::spawn(move || {
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        // progress lines end with \r; keep the last state of the line
                        let text = strip_ansi(line.rsplit('\r').next().unwrap_or(&line))
                            .trim_end()
                            .to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let _ = tx.send(Msg::Line {
                            text,
                            stderr: is_err,
                        });
                        ctx.request_repaint();
                    }
                }));
            }
            let status = child.wait();
            for r in readers {
                let _ = r.join();
            }
            let (ok, code) = match status {
                Ok(s) => (s.success(), s.code()),
                Err(_) => (false, None),
            };
            let _ = tx.send(Msg::Exit {
                label: job.label,
                ok,
                code,
                elapsed_secs: t0.elapsed().as_secs_f32(),
            });
            ctx.request_repaint();
        });
        true
    }
}

/// Drop `ESC [ … <letter>` sequences (makepkg and friends colour their output regardless).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\x07' {
            out.push(c);
        }
    }
    out
}

/// Merge a check result into the inventory: set `latest` on the installed packages.
pub fn apply_updates(packages: &mut [Package], updates: &[Upgrade]) {
    let map: HashMap<&str, &Upgrade> = updates.iter().map(|u| (u.name.as_str(), u)).collect();
    for p in packages.iter_mut() {
        p.latest = map.get(p.name.as_str()).map(|u| u.new.clone());
    }
}

/// The saved check (start-up, before the first live check).
pub fn saved_check() -> CheckState {
    state::load()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_and_bell() {
        assert_eq!(strip_ansi("\x1b[1;34m==>\x1b[0m done\x07"), "==> done");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn command_line_shows_file_names() {
        let j = Job {
            label: "x".into(),
            program: "/usr/bin/pkexec".into(),
            args: vec![
                "/usr/bin/pacman".into(),
                "-S".into(),
                "--noconfirm".into(),
                "rg".into(),
            ],
        };
        assert_eq!(j.command_line(), "pkexec pacman -S --noconfirm rg");
    }
}
