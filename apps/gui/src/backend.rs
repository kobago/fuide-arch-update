//! Worker threads around `archpkg`: inventory, update checks, searches and detail lookups run
//! off the UI thread and come back as [`Msg`]. Mutations are not here — they go through the
//! console (`pty.rs`) because `sudo` and pacman ask questions.

use std::collections::HashMap;
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
    pub privilege: Option<String>,
    pub checkupdates: bool,
    pub tray_running: bool,
}

pub enum Msg {
    /// `pacman -Qi` (+ foreign / orphan marks). Elapsed ms.
    Inventory(Result<Vec<Package>, String>, f32),
    /// `checkupdates` + `<helper> -Qua`; the state file has been rewritten.
    Check(Result<Vec<Upgrade>, String>),
    /// Repo + AUR search hits for `query`.
    Search {
        query: String,
        result: Result<Vec<SearchHit>, String>,
    },
    /// `-Si` details for packages that are not installed (search results).
    Details(Result<Vec<Package>, String>),
    System(SystemInfo),
}

pub struct Backend {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    fetching: bool,
    checking: bool,
    searching: Option<String>,
    details_pending: usize,
    system_pending: usize,
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
    /// Anything in flight (tests wait on this).
    #[allow(dead_code)]
    pub fn busy(&self) -> bool {
        self.fetching
            || self.checking
            || self.searching.is_some()
            || self.details_pending > 0
            || self.system_pending > 0
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
                    // AUR RPC down: keep the repo hits, the UI shows the error
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
                tray_running: std::process::Command::new("pgrep")
                    .args(["-x", "fuide-arch-update-tray"])
                    .output()
                    .is_ok_and(|o| !o.stdout.is_empty()),
            };
            let _ = tx.send(Msg::System(info));
            ctx.request_repaint();
        });
    }
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
