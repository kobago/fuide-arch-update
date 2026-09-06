//! FUIDE Arch-Update — pacman / AUR package manager with a tactical-console look.
//!
//! Left: views + system readout. Centre: package table. Right: package inspector. Bottom:
//! event log. Commands that change the system run non-interactively through polkit
//! (`pkexec`, the desktop's own password dialog) and stream their output into the log.

use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{pos2, vec2, Align2, Key, Rect, RichText, Sense, Ui};
use fuide::table::{self, Cell, Column, TableState, Width};
use fuide::widgets::{self, LogLine};
use fuide::{
    mono, palette, theme, type_scale, Dialog, PaletteKind, Panel, Settings, SettingsWindow, Shell,
};

use archpkg::pacman::{self, Package, Reason, SearchHit};
use archpkg::Upgrade;

use crate::backend::{self, Backend, Job, Msg, SystemInfo};

pub const APP_ID: &str = "arch-update";
pub const APP_NAME: &str = "FUIDE Arch-Update";

const LEFT_W: f32 = 236.0;
const RIGHT_W: f32 = 340.0;
const GAP: f32 = 14.0;
const TOOLBAR_H: f32 = 32.0;
/// Default log panel height; the divider above it is draggable (`Settings::log_height`).
const LOG_H: f32 = 170.0;
const LOG_MIN: f32 = 60.0;
/// Header strip left when the log panel is collapsed (`Settings::log_open` = false).
const LOG_CLOSED: f32 = 26.0;
/// Space kept for the panels above the log when the divider is dragged up.
const BODY_MIN: f32 = 360.0;
const VIEWS_H: f32 = 268.0;

/// Command-line start-up requests (from the tray applet).
#[derive(Clone, Debug, Default)]
pub struct StartUp {
    pub upgrade: bool,
    pub select: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Installed,
    Explicit,
    Updates,
    Foreign,
    Orphans,
    Search,
}

impl View {
    pub const ALL: [View; 6] = [
        View::Installed,
        View::Explicit,
        View::Updates,
        View::Foreign,
        View::Orphans,
        View::Search,
    ];
    pub fn label(self) -> &'static str {
        match self {
            View::Installed => "Installed",
            View::Explicit => "Explicit",
            View::Updates => "Updates",
            View::Foreign => "AUR",
            View::Orphans => "Orphans",
            View::Search => "Search",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Danger,
}

pub struct Event {
    pub time: String,
    pub text: String,
    pub level: Level,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Outdated,
    Orphan,
    Explicit,
    Dependency,
    Available,
}

impl Status {
    pub fn tag(self) -> &'static str {
        match self {
            Status::Outdated => "OUTDATED",
            Status::Orphan => "ORPHAN",
            Status::Explicit => "EXPLICIT",
            Status::Dependency => "DEP",
            Status::Available => "AVAILABLE",
        }
    }
}

pub fn status_of(p: &Package) -> Status {
    if !p.installed {
        Status::Available
    } else if p.outdated() {
        Status::Outdated
    } else if p.orphan {
        Status::Orphan
    } else if p.reason == Reason::Dependency {
        Status::Dependency
    } else {
        Status::Explicit
    }
}

/// A confirmation before running a mutating command.
pub struct Confirm {
    pub title: String,
    pub line: String,
    pub note: String,
    pub verb: String,
    pub danger: bool,
    pub job: Job,
}

pub enum DialogState {
    Confirm(Confirm),
    /// Big `ERROR` / `SUCCESS` card; details live in the event log.
    Notice {
        success: bool,
        line: String,
    },
}

pub struct OpenDialog {
    pub state: DialogState,
    pub closing: bool,
}

pub enum Action {
    SetView(View),
    Select(Option<usize>),
    Refresh,
    Check,
    Search,
    UpgradeAll,
    Install(String, bool),
    Remove(String),
    /// Rebuild / reinstall an AUR package through the helper (its upgrade path).
    UpgradeAur(String),
    MarkExplicit(String, bool),
    RemoveOrphans,
    CleanCache,
    Homepage(String),
    /// Open the package's PKGBUILD on aur.archlinux.org (review before building).
    Pkgbuild(String),
    CopyName(String),
    Run(Job),
    CloseDialog,
    ConfirmDialog,
    OpenSettings,
}

pub struct PkgApp {
    pub backend: Backend,
    pub packages: Vec<Package>,
    pub updates: Vec<Upgrade>,
    pub checked_at: Option<u64>,
    pub check_error: Option<String>,
    pub search_results: Vec<Package>,
    pub search_query: String,
    pub system: SystemInfo,
    pub view: View,
    pub rows: Vec<usize>,
    table: TableState,
    dirty: bool,
    filter: String,
    pub log: Vec<Event>,
    pub dialog: Option<OpenDialog>,
    notice_queue: VecDeque<(bool, String)>,
    fetch_ms: f32,
    /// Names whose `-Si` details were requested (search results).
    details_requested: std::collections::HashSet<String>,
    start: StartUp,
    /// Lock + socket of the one instance per session (`instance.rs`); requests from later
    /// launches arrive here.
    instance: Option<crate::instance::Guard>,
    devshot: fuide::devshot::DevShot,
    agent: fuide::Agent,
    dev_dialog: Option<String>,
    dev_search: Option<String>,
    dev_frame: u32,
    dev_close_frame: Option<u32>,
    pub settings: Settings,
    settings_win: SettingsWindow,
    settings_path: Option<PathBuf>,
    /// Current log panel height (draggable divider).
    log_h: f32,
}

impl PkgApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        start: StartUp,
        mut instance: crate::instance::Guard,
    ) -> Self {
        let settings = Settings::load(APP_ID).unwrap_or_else(|| Settings::new(PaletteKind::Cyan));
        let mut app = Self::with_context(&cc.egui_ctx, settings);
        app.settings_path = Settings::path(APP_ID);
        app.start = start;
        instance.serve(cc.egui_ctx.clone());
        app.instance = Some(instance);
        app.load_saved_check();
        app.backend.fetch_inventory(cc.egui_ctx.clone());
        app.backend.fetch_system(cc.egui_ctx.clone());
        // a live check at start (network); the tray does the same on its timer
        app.backend.check(cc.egui_ctx.clone());
        app
    }

    pub fn with_context(ctx: &egui::Context, settings: Settings) -> Self {
        theme::install(ctx, settings.palette.palette(), cjk_fallback());
        settings.apply(ctx);
        let mut app = Self {
            backend: Backend::new(),
            packages: Vec::new(),
            updates: Vec::new(),
            checked_at: None,
            check_error: None,
            search_results: Vec::new(),
            search_query: String::new(),
            system: SystemInfo::default(),
            view: View::Installed,
            rows: Vec::new(),
            table: TableState::default(),
            dirty: true,
            filter: String::new(),
            log: Vec::new(),
            dialog: None,
            notice_queue: VecDeque::new(),
            fetch_ms: 0.0,
            details_requested: Default::default(),
            start: StartUp::default(),
            instance: None,
            devshot: fuide::devshot::DevShot::from_env(),
            agent: fuide::Agent::new(APP_ID, APP_NAME),
            dev_dialog: std::env::var("FUIDE_DEV_DIALOG").ok(),
            dev_search: std::env::var("FUIDE_DEV_SEARCH").ok(),
            dev_frame: 0,
            dev_close_frame: std::env::var("FUIDE_DEV_DIALOG_CLOSE")
                .ok()
                .and_then(|v| v.parse().ok()),
            log_h: settings.log_height.unwrap_or(LOG_H),
            settings,
            settings_win: SettingsWindow::default(),
            settings_path: None,
        };
        app.push_log(
            0.0,
            "package console online :: reading inventory",
            Level::Ok,
        );
        app.agent.set_enabled(ctx, app.settings.agent);
        if app.settings.agent {
            app.push_log(
                0.0,
                "agent // interface on :: waiting for a client",
                Level::Warn,
            );
        }
        if std::env::var_os("FUIDE_DEV_SETTINGS").is_some() {
            app.settings_win.open();
        }
        if let Ok(text) = std::env::var("FUIDE_DEV_LOG") {
            app.push_log(0.0, text, Level::Danger);
        }
        app
    }

    /// The tray's last check (shown until the live check lands).
    pub fn load_saved_check(&mut self) {
        let st = backend::saved_check();
        self.updates = st.updates;
        self.checked_at = st.checked_at;
        self.check_error = st.error;
        backend::apply_updates(&mut self.packages, &self.updates);
        self.dirty = true;
    }

    // ------------------------------------------------------------------ state

    fn push_log(&mut self, t: f64, text: impl Into<String>, level: Level) {
        self.log.push(Event {
            time: format!("[{}]", fuide::fmt::uptime(t)),
            text: text.into(),
            level,
        });
        if self.log.len() > 3000 {
            self.log.drain(..1000);
        }
    }

    fn fail(&mut self, t: f64, line: impl Into<String>, detail: &str) {
        let line = line.into();
        self.push_log(t, format!("{line} :: failed :: {detail}"), Level::Danger);
        self.notice_queue.push_back((false, line));
    }

    fn succeed(&mut self, t: f64, line: impl Into<String>, detail: &str) {
        let line = line.into();
        self.push_log(t, format!("{line} :: {detail}"), Level::Ok);
        self.notice_queue.push_back((true, line));
    }

    pub fn source(&self) -> &[Package] {
        if self.view == View::Search {
            &self.search_results
        } else {
            &self.packages
        }
    }

    pub fn selected_package(&self) -> Option<&Package> {
        self.table
            .selected
            .and_then(|r| self.rows.get(r))
            .and_then(|&i| self.source().get(i))
    }

    pub fn outdated_count(&self) -> usize {
        self.updates.len()
    }

    pub fn count(&self, v: View) -> usize {
        match v {
            View::Installed => self.packages.len(),
            View::Explicit => self
                .packages
                .iter()
                .filter(|p| p.reason == Reason::Explicit)
                .count(),
            View::Updates => self.updates.len(),
            View::Foreign => self.packages.iter().filter(|p| p.is_foreign()).count(),
            View::Orphans => self.packages.iter().filter(|p| p.orphan).count(),
            View::Search => self.search_results.len(),
        }
    }

    pub fn rebuild_rows(&mut self) {
        let filter = self.filter.to_lowercase();
        let src = self.source();
        let mut rows: Vec<usize> = src
            .iter()
            .enumerate()
            .filter(|(_, p)| match self.view {
                View::Installed | View::Search => true,
                View::Explicit => p.reason == Reason::Explicit,
                View::Updates => p.outdated(),
                View::Foreign => p.is_foreign(),
                View::Orphans => p.orphan,
            })
            .filter(|(_, p)| {
                filter.is_empty()
                    || p.name.to_lowercase().contains(&filter)
                    || p.desc.to_lowercase().contains(&filter)
            })
            .map(|(i, _)| i)
            .collect();
        let (col, desc) = (self.table.sort_col, self.table.sort_desc);
        rows.sort_by(|&a, &b| {
            let (pa, pb) = (&src[a], &src[b]);
            let ord = match col {
                1 => pa.version.cmp(&pb.version),
                2 => pa.latest.cmp(&pb.latest),
                3 => pa.repo.cmp(&pb.repo),
                4 => status_of(pa).cmp(&status_of(pb)),
                5 => pa
                    .installed_size_kib
                    .partial_cmp(&pb.installed_size_kib)
                    .unwrap_or(std::cmp::Ordering::Equal),
                _ => std::cmp::Ordering::Equal,
            }
            .then_with(|| pa.name.to_lowercase().cmp(&pb.name.to_lowercase()));
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        let keep = self
            .table
            .selected
            .and_then(|r| self.rows.get(r).copied())
            .and_then(|old| rows.iter().position(|&i| i == old));
        self.rows = rows;
        self.table.selected = keep;
        self.dirty = false;
    }

    /// While a confirmation is open and the agent may not confirm, its verb is human-only.
    fn agent_blocked(&self) -> Vec<String> {
        match &self.dialog {
            Some(OpenDialog {
                state: DialogState::Confirm(c),
                closing: false,
            }) if !self.settings.agent_confirm => vec![c.verb.clone()],
            _ => Vec::new(),
        }
    }

    fn agent_state(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "view: {} :: rows: {} :: filter: {:?} :: pending updates: {}",
            self.view.label().to_uppercase(),
            self.rows.len(),
            self.filter,
            self.updates.len()
        );
        match self.selected_package() {
            Some(p) => {
                let _ = writeln!(
                    s,
                    "selected: {} {} ({}) :: {}{}",
                    p.name,
                    p.version,
                    p.repo,
                    status_of(p).tag(),
                    p.latest
                        .as_ref()
                        .map(|l| format!(" -> {l}"))
                        .unwrap_or_default()
                );
            }
            None => s.push_str("selected: none\n"),
        }
        if let Some(j) = self.backend.running() {
            let _ = writeln!(s, "running: {} ({})", j.label, j.command_line());
        }
        if let Some(d) = self.dialog.as_ref().filter(|d| !d.closing) {
            match &d.state {
                DialogState::Confirm(c) => {
                    let _ = writeln!(
                        s,
                        "dialog: CONFIRM {} :: {} :: {} :: buttons CANCEL / {}",
                        c.title.to_uppercase(),
                        c.line,
                        c.job.command_line(),
                        c.verb
                    );
                }
                DialogState::Notice { success, line } => {
                    let _ = writeln!(
                        s,
                        "dialog: {} :: {line} :: press ACKNOWLEDGE",
                        if *success { "SUCCESS" } else { "ERROR" }
                    );
                }
            }
        }
        s.push_str("log (latest last):\n");
        let skip = self.log.len().saturating_sub(8);
        for e in &self.log[skip..] {
            let _ = writeln!(s, "  {} {}", e.time, e.text);
        }
        s
    }

    pub fn poll(&mut self, ctx: &egui::Context, t: f64) {
        for msg in self.backend.poll() {
            match msg {
                Msg::Inventory(result, ms) => {
                    self.fetch_ms = ms;
                    match result {
                        Ok(mut pkgs) => {
                            backend::apply_updates(&mut pkgs, &self.updates);
                            let explicit =
                                pkgs.iter().filter(|p| p.reason == Reason::Explicit).count();
                            let foreign = pkgs.iter().filter(|p| p.is_foreign()).count();
                            let orphans = pkgs.iter().filter(|p| p.orphan).count();
                            self.push_log(
                                t,
                                format!(
                                    "inventory :: {} packages, {explicit} explicit, {foreign} foreign, {orphans} orphans in {ms:.0} ms",
                                    pkgs.len()
                                ),
                                if orphans > 0 { Level::Warn } else { Level::Ok },
                            );
                            self.packages = pkgs;
                            self.after_inventory();
                        }
                        Err(e) => self.fail(t, "INVENTORY // PACMAN", &e),
                    }
                    self.dirty = true;
                }
                Msg::Check(result) => {
                    self.checked_at = Some(archpkg::now_epoch());
                    match result {
                        Ok(u) => {
                            let aur = u.iter().filter(|x| x.aur).count();
                            self.push_log(
                                t,
                                format!(
                                    "check :: {} updates ({} repo, {aur} aur)",
                                    u.len(),
                                    u.len() - aur
                                ),
                                if u.is_empty() { Level::Ok } else { Level::Warn },
                            );
                            self.updates = u;
                            self.check_error = None;
                            backend::apply_updates(&mut self.packages, &self.updates);
                            self.dirty = true;
                        }
                        Err(e) => {
                            self.check_error = Some(e.clone());
                            self.fail(t, "CHECK // UPDATES", &e);
                        }
                    }
                }
                Msg::Search { query, result } => match result {
                    Ok(hits) => {
                        let pkgs = self.hits_to_packages(hits);
                        self.push_log(
                            t,
                            format!("search // {query} :: {} hits", pkgs.len()),
                            Level::Ok,
                        );
                        self.search_results = pkgs;
                        self.details_requested.clear();
                        self.dirty = true;
                    }
                    Err(e) => self.fail(t, format!("SEARCH // {}", query.to_uppercase()), &e),
                },
                Msg::Details(result) => match result {
                    Ok(pkgs) => {
                        for d in pkgs {
                            if let Some(p) = self.search_results.iter_mut().find(|p| {
                                p.name == d.name && (p.repo == d.repo || p.repo.is_empty())
                            }) {
                                let (installed, latest) = (p.installed, p.latest.clone());
                                *p = d;
                                p.installed = installed;
                                p.latest = latest;
                            }
                        }
                    }
                    Err(e) => self.push_log(t, format!("details :: failed :: {e}"), Level::Warn),
                },
                Msg::System(info) => self.system = info,
                Msg::Line { text, stderr } => {
                    let lower = text.to_lowercase();
                    let level = if lower.starts_with("error") || lower.contains("error:") {
                        Level::Danger
                    } else if lower.starts_with("warning") || lower.contains("warning:") {
                        Level::Warn
                    } else if text.starts_with("::") || text.starts_with("==>") {
                        Level::Ok
                    } else {
                        let _ = stderr; // makepkg writes progress to stderr; not an error by itself
                        Level::Info
                    };
                    self.push_log(t, text, level);
                }
                Msg::Exit {
                    label,
                    ok,
                    code,
                    elapsed_secs,
                } => {
                    let time = format!("{elapsed_secs:.1} s");
                    if ok {
                        self.succeed(t, label.to_uppercase(), &format!("done in {time}"));
                    } else {
                        let detail = match code {
                            // pkexec: 126 = dismissed, 127 = not authorised
                            Some(126) => "authentication dismissed".to_string(),
                            Some(127) => "not authorised (polkit) or command not found".to_string(),
                            Some(c) => format!("exit code {c}"),
                            None => "terminated".to_string(),
                        };
                        self.fail(t, label.to_uppercase(), &detail);
                    }
                    // the package database may have changed: re-read and re-check (the state
                    // file tells the tray)
                    self.backend.fetch_inventory(ctx.clone());
                    self.backend.fetch_system(ctx.clone());
                    self.backend.check(ctx.clone());
                }
            }
        }
    }

    /// Requests from later launches (the tray's click while the window is open): apply them
    /// like start-up flags and bring the window to the front.
    fn poll_instance(&mut self, ctx: &egui::Context) {
        let requests = match self.instance.as_mut() {
            Some(g) => g.drain(),
            None => return,
        };
        if requests.is_empty() {
            return;
        }
        for r in requests {
            match r {
                crate::instance::Request::Show => {}
                crate::instance::Request::Upgrade => self.start.upgrade = true,
                crate::instance::Request::Select(name) => self.start.select = Some(name),
            }
        }
        // a request that needs the inventory waits for it when it is not in yet (see `poll`);
        // a full-upgrade request never interrupts a running command or an open dialog
        if !self.packages.is_empty() {
            if self.start.upgrade && (self.dialog.is_some() || self.backend.running().is_some()) {
                self.start.upgrade = false;
            }
            self.after_inventory();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Start-up requests that need the inventory: `--select`, `--upgrade`.
    fn after_inventory(&mut self) {
        if let Some(name) = self.start.select.take() {
            self.view = View::Updates;
            self.filter.clear();
            self.rebuild_rows();
            let mut pos = self
                .rows
                .iter()
                .position(|&i| self.packages[i].name == name);
            if pos.is_none() {
                self.view = View::Installed;
                self.rebuild_rows();
                pos = self
                    .rows
                    .iter()
                    .position(|&i| self.packages[i].name == name);
            }
            self.table.selected = pos;
            self.table.scroll_to_selected = true;
        }
        if self.start.upgrade {
            self.start.upgrade = false;
            self.open_upgrade_all();
        }
    }

    fn hits_to_packages(&self, hits: Vec<SearchHit>) -> Vec<Package> {
        let mut out: Vec<Package> = Vec::new();
        for h in hits {
            // the same name may come from several repos (cachyos-v3 + extra): keep the first
            if out.iter().any(|p| p.name == h.name) {
                continue;
            }
            if let Some(inst) = self.packages.iter().find(|p| p.name == h.name) {
                out.push(inst.clone());
                continue;
            }
            out.push(Package {
                name: h.name,
                version: h.version,
                desc: h.desc,
                repo: h.repo,
                installed: false,
                votes: h.votes,
                popularity: h.popularity,
                out_of_date: h.out_of_date,
                ..Default::default()
            });
        }
        out
    }

    /// Ask for `-Si` details of a not-installed search result the first time it is selected.
    fn want_details(&mut self, ctx: &egui::Context) {
        let Some(p) = self.selected_package() else {
            return;
        };
        if p.installed || !p.url.is_empty() || self.details_requested.contains(&p.name) {
            return;
        }
        let (name, aur) = (p.name.clone(), p.is_aur());
        self.details_requested.insert(name.clone());
        if aur {
            self.backend.details(Vec::new(), vec![name], ctx.clone());
        } else {
            self.backend.details(vec![name], Vec::new(), ctx.clone());
        }
    }

    fn confirm(&mut self, c: Confirm) {
        self.dialog = Some(OpenDialog {
            state: DialogState::Confirm(c),
            closing: false,
        });
    }

    /// `pkexec <program> <args>`: polkit asks for the password in the desktop's own dialog.
    fn root_job(&self, label: &str, program: PathBuf, args: &[&str]) -> Result<Job, String> {
        let su = pacman::privilege_cmd().ok_or_else(|| {
            "pkexec not found (install polkit and a polkit authentication agent)".to_string()
        })?;
        let mut full = vec![program.display().to_string()];
        full.extend(args.iter().map(|s| s.to_string()));
        Ok(Job {
            label: label.into(),
            program: su,
            args: full,
        })
    }

    fn pacman_job(&self, label: &str, args: &[&str]) -> Result<Job, String> {
        self.root_job(label, pacman::pacman_bin(), args)
    }

    /// `<helper> <args> --noconfirm --sudo pkexec`: the helper runs as the user and asks
    /// polkit for root when it needs pacman.
    fn helper_job(&self, label: &str, args: &[&str]) -> Result<Job, String> {
        let helper = pacman::aur_helper()
            .ok_or_else(|| "no AUR helper installed (yay, paru, pikaur)".to_string())?;
        let su = pacman::privilege_cmd().ok_or_else(|| {
            "pkexec not found (install polkit and a polkit authentication agent)".to_string()
        })?;
        let mut full: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        full.push("--noconfirm".into());
        full.extend(pacman::helper_sudo_args(&helper, &su));
        Ok(Job {
            label: label.into(),
            program: helper.display().to_string(),
            args: full,
        })
    }

    fn open_upgrade_all(&mut self) {
        let n = self.updates.len();
        let aur = self.updates.iter().filter(|u| u.aur).count();
        let job = if pacman::aur_helper().is_some() {
            self.helper_job("upgrade // all", &["-Syu"])
        } else {
            self.pacman_job("upgrade // all", &["-Syu", "--noconfirm", "--ask", "4"])
        };
        match job {
            Ok(job) => self.confirm(Confirm {
                title: "Full upgrade".into(),
                line: format!("{n} packages ({} repo, {aur} aur)", n - aur),
                note: "SYNCS THE DATABASES AND UPGRADES EVERYTHING :: NO QUESTIONS ARE ASKED (--noconfirm)".into(),
                verb: "UPGRADE ALL".into(),
                danger: false,
                job,
            }),
            Err(e) => {
                self.notice_queue.push_back((false, "UPGRADE // ALL".into()));
                self.push_log(0.0, format!("upgrade // all :: failed :: {e}"), Level::Danger);
            }
        }
    }

    pub fn apply(&mut self, ctx: &egui::Context, action: Action, t: f64) {
        match action {
            Action::SetView(v) => {
                if self.view != v {
                    self.view = v;
                    self.table.selected = None;
                    self.dirty = true;
                }
            }
            Action::Select(s) => {
                self.table.selected = s;
                self.table.scroll_to_selected = true;
                self.want_details(ctx);
            }
            Action::Refresh => {
                self.push_log(t, "refresh // inventory", Level::Info);
                self.backend.fetch_inventory(ctx.clone());
                self.backend.fetch_system(ctx.clone());
            }
            Action::Check => {
                self.push_log(t, "check // checkupdates + aur", Level::Info);
                self.backend.check(ctx.clone());
            }
            Action::Search => {
                let q = self.search_query.trim().to_string();
                if q.len() < 2 {
                    return;
                }
                self.push_log(t, format!("search // {q}"), Level::Info);
                self.backend.search(q, ctx.clone());
            }
            Action::UpgradeAll => self.open_upgrade_all(),
            Action::Install(name, aur) => {
                let job = if aur {
                    self.helper_job(&format!("install // {name}"), &["-S", &name])
                } else {
                    // `--ask 4`: a package that conflicts with the new one is replaced (pacman
                    // would otherwise answer its own "Remove X?" question with no)
                    self.pacman_job(
                        &format!("install // {name}"),
                        &["-S", "--needed", "--noconfirm", "--ask", "4", &name],
                    )
                };
                match job {
                    Ok(job) => self.confirm(Confirm {
                        title: "Install".into(),
                        line: name.clone(),
                        note: if aur {
                            "BUILT FROM THE AUR WITHOUT REVIEW :: READ THE PKGBUILD FIRST (BUTTON IN THE INSPECTOR)".into()
                        } else {
                            "DEPENDENCIES ARE INSTALLED AS NEEDED :: A CONFLICTING PACKAGE IS REPLACED".into()
                        },
                        verb: "INSTALL".into(),
                        danger: false,
                        job,
                    }),
                    Err(e) => self.fail(t, format!("INSTALL // {}", name.to_uppercase()), &e),
                }
            }
            Action::Remove(name) => {
                match self.pacman_job(
                    &format!("remove // {name}"),
                    &["-Rns", "--noconfirm", &name],
                ) {
                    Ok(job) => self.confirm(Confirm {
                        title: "Remove".into(),
                        line: name.clone(),
                        note: "REMOVES THE PACKAGE, ITS CONFIG AND UNNEEDED DEPENDENCIES (-Rns)"
                            .into(),
                        verb: "REMOVE".into(),
                        danger: true,
                        job,
                    }),
                    Err(e) => self.fail(t, format!("REMOVE // {}", name.to_uppercase()), &e),
                }
            }
            Action::UpgradeAur(name) => {
                match self.helper_job(&format!("upgrade // {name}"), &["-S", &name]) {
                    Ok(job) => self.confirm(Confirm {
                        title: "Upgrade".into(),
                        line: name.clone(),
                        note: "REBUILT FROM THE AUR WITHOUT REVIEW :: READ THE PKGBUILD FIRST"
                            .into(),
                        verb: "UPGRADE".into(),
                        danger: false,
                        job,
                    }),
                    Err(e) => self.fail(t, format!("UPGRADE // {}", name.to_uppercase()), &e),
                }
            }
            Action::MarkExplicit(name, explicit) => {
                let flag = if explicit { "--asexplicit" } else { "--asdeps" };
                match self.pacman_job(&format!("mark // {name}"), &["-D", flag, &name]) {
                    Ok(job) => self.apply(ctx, Action::Run(job), t),
                    Err(e) => self.fail(t, format!("MARK // {}", name.to_uppercase()), &e),
                }
            }
            Action::RemoveOrphans => {
                let names: Vec<String> = self
                    .packages
                    .iter()
                    .filter(|p| p.orphan)
                    .map(|p| p.name.clone())
                    .collect();
                if names.is_empty() {
                    return;
                }
                let mut args = vec!["-Rns", "--noconfirm"];
                args.extend(names.iter().map(String::as_str));
                match self.pacman_job("remove // orphans", &args) {
                    Ok(job) => self.confirm(Confirm {
                        title: "Remove orphans".into(),
                        line: format!("{} packages", names.len()),
                        note: names.join("  "),
                        verb: "REMOVE".into(),
                        danger: true,
                        job,
                    }),
                    Err(e) => self.fail(t, "REMOVE // ORPHANS", &e),
                }
            }
            Action::CleanCache => {
                match self.root_job("clean // cache", pacman::paccache_bin(), &["-rk2"]) {
                    Ok(job) => self.confirm(Confirm {
                        title: "Clean cache".into(),
                        line: self
                            .system
                            .cache_kib
                            .map(|k| {
                                format!("{} in /var/cache/pacman/pkg", archpkg::fmt_kib(k).trim())
                            })
                            .unwrap_or_else(|| "package cache".into()),
                        note: "KEEPS THE LAST 2 VERSIONS OF EACH PACKAGE (paccache -rk2)".into(),
                        verb: "CLEAN".into(),
                        danger: false,
                        job,
                    }),
                    Err(e) => self.fail(t, "CLEAN // CACHE", &e),
                }
            }
            Action::Homepage(url) => {
                self.push_log(t, format!("open // {url}"), Level::Info);
                if let Err(e) = open::that_detached(&url) {
                    self.fail(t, "OPEN // HOMEPAGE", &e.to_string());
                }
            }
            Action::Pkgbuild(name) => {
                let url = format!("https://aur.archlinux.org/cgit/aur.git/tree/PKGBUILD?h={name}");
                self.apply(ctx, Action::Homepage(url), t);
            }
            Action::CopyName(name) => {
                ctx.copy_text(name);
                self.push_log(t, "name copied to clipboard", Level::Info);
            }
            Action::Run(job) => {
                let line = job.command_line();
                let label = job.label.clone();
                if self.backend.run(job, ctx.clone()) {
                    self.push_log(t, format!("$ {line}"), Level::Info);
                    self.settings.log_open = true;
                } else {
                    self.fail(t, label.to_uppercase(), "another command is still running");
                }
            }
            Action::CloseDialog => {
                if let Some(d) = &mut self.dialog {
                    d.closing = true;
                }
            }
            Action::ConfirmDialog => {
                let Some(OpenDialog {
                    state: DialogState::Confirm(c),
                    closing,
                }) = &mut self.dialog
                else {
                    return;
                };
                if *closing {
                    return;
                }
                *closing = true;
                let job = c.job.clone();
                self.apply(ctx, Action::Run(job), t);
            }
            Action::OpenSettings => self.settings_win.open(),
        }
    }

    fn settings_changed(&mut self, t: f64) {
        let s = &self.settings;
        self.push_log(
            t,
            format!(
                "settings // palette {} :: {} :: {} :: {} :: agent {}{}",
                s.palette.name(),
                if s.chamfer { "chamfer" } else { "square" },
                if s.compact { "compact" } else { "normal" },
                if s.transparent {
                    "translucent"
                } else {
                    "opaque"
                },
                if s.agent { "on" } else { "off" },
                if s.agent && s.agent_confirm {
                    " (may confirm)"
                } else {
                    ""
                }
            ),
            Level::Warn,
        );
        self.save_settings(t);
    }

    fn save_settings(&mut self, t: f64) {
        if let Some(path) = self.settings_path.clone() {
            if let Err(e) = self.settings.save_to(&path) {
                self.push_log(t, format!("settings // save failed: {e}"), Level::Danger);
            }
        }
    }

    fn handle_keys(&self, ui: &Ui, actions: &mut Vec<Action>) {
        if self.dialog.is_some() {
            return;
        }
        let focused = ui.memory(|m| m.focused().is_some());
        ui.input(|i| {
            let cmd = i.modifiers.command;
            if cmd && i.key_pressed(Key::Comma) {
                actions.push(Action::OpenSettings);
            }
            if cmd && i.key_pressed(Key::R) {
                actions.push(Action::Refresh);
            }
            if cmd && i.key_pressed(Key::U) {
                actions.push(Action::Check);
            }
            for (n, key) in [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
            ]
            .iter()
            .enumerate()
            {
                if cmd && i.key_pressed(*key) {
                    actions.push(Action::SetView(View::ALL[n]));
                }
            }
            if focused {
                return;
            }
            if i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowUp) {
                let dir: isize = if i.key_pressed(Key::ArrowDown) { 1 } else { -1 };
                let next = match self.table.selected {
                    Some(p) => (p as isize + dir).clamp(0, self.rows.len() as isize - 1) as usize,
                    None => 0,
                };
                if !self.rows.is_empty() {
                    actions.push(Action::Select(Some(next)));
                }
            }
            if i.key_pressed(Key::Enter) {
                if let Some(p) = self.selected_package() {
                    if !p.url.is_empty() {
                        actions.push(Action::Homepage(p.url.clone()));
                    }
                }
            }
            if cmd && i.key_pressed(Key::Backspace) {
                if let Some(p) = self.selected_package() {
                    if p.installed {
                        actions.push(Action::Remove(p.name.clone()));
                    }
                }
            }
        });
    }
}

/// A Japanese-capable system font as a fallback, found through fontconfig.
fn cjk_fallback() -> Vec<theme::FallbackFont> {
    let Ok(o) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}\n", "sans-serif:lang=ja"])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&o.stdout);
    let mut lines = text.lines();
    let (Some(file), index) = (
        lines.next(),
        lines
            .next()
            .and_then(|i| i.trim().parse().ok())
            .unwrap_or(0),
    ) else {
        return Vec::new();
    };
    match std::fs::read(file.trim()) {
        Ok(bytes) => vec![theme::FallbackFont {
            name: "system-cjk".into(),
            bytes,
            index,
        }],
        Err(_) => Vec::new(),
    }
}

fn cmd_glyph() -> &'static str {
    if cfg!(target_os = "macos") {
        "CMD+"
    } else {
        "CTRL+"
    }
}

// ---------------------------------------------------------------------------- UI

impl eframe::App for PkgApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.devshot.tick(ui.ctx());
        self.agent.set_enabled(ui.ctx(), self.settings.agent);
        self.agent.set_blocked(self.agent_blocked());
        let agent_state = self.agent.wants_state().then(|| self.agent_state());
        self.agent.tick(ui.ctx(), agent_state);
        let t = ui.input(|i| i.time);
        let ctx = ui.ctx().clone();
        self.poll_instance(&ctx);
        self.poll(&ctx, t);
        if self.dirty {
            self.rebuild_rows();
        }
        let pal = palette(ui.ctx());
        let fps = 1.0 / ui.input(|i| i.stable_dt).max(1e-3);

        let mut actions: Vec<Action> = Vec::new();
        if self.dialog.is_none() {
            if let Some((success, line)) = self.notice_queue.pop_front() {
                self.dialog = Some(OpenDialog {
                    state: DialogState::Notice { success, line },
                    closing: false,
                });
            }
        }
        self.dev_frame += 1;
        if let Some(q) = self.dev_search.take() {
            self.search_query = q;
            actions.push(Action::SetView(View::Search));
            actions.push(Action::Search);
        }
        if let Some(kind) = self.dev_dialog.take_if(|_| !self.packages.is_empty()) {
            self.dev_dialog_state(kind.as_str(), &mut actions);
        }
        if self.dev_close_frame == Some(self.dev_frame) && self.dialog.is_some() {
            actions.push(Action::CloseDialog);
        }
        self.handle_keys(ui, &mut actions);

        let outdated = self.outdated_count();
        let (link_text, link_color) = if self.fetch_ms == 0.0 {
            ("PACMAN PROBING", pal.text_dim)
        } else if self.packages.is_empty() {
            ("PACMAN FAILED", pal.danger)
        } else {
            ("PACMAN OK", pal.ok)
        };
        let mut shell = Shell::new(APP_NAME)
            .subtitle(format!(
                "{} :: {}",
                self.system
                    .aur_helper
                    .as_deref()
                    .map(|h| format!("pacman + {h}"))
                    .unwrap_or_else(|| "pacman".into()),
                if self.system.kernel.is_empty() {
                    "linux"
                } else {
                    &self.system.kernel
                }
            ))
            .status_left(format!(
                "{} :: {} ROWS :: {} OUTDATED :: {:.0} FPS :: INVENTORY {:.0} MS",
                fuide::fmt::uptime(t),
                self.rows.len(),
                outdated,
                fps,
                self.fetch_ms,
            ))
            .lamp(link_text, link_color, false)
            .settings_button(true);
        if self.system.reboot_required {
            shell = shell.lamp("REBOOT REQUIRED", pal.danger, false);
        }
        if outdated > 0 {
            shell = shell.lamp(format!("{outdated} UPDATES"), pal.warn, false);
        }
        if self.backend.fetching() {
            shell = shell.lamp("INVENTORY", pal.warn, true);
        }
        if self.backend.checking() {
            shell = shell.lamp("CHECKING", pal.warn, true);
        }
        if self.backend.searching().is_some() {
            shell = shell.lamp("SEARCHING", pal.warn, true);
        }
        if let Some(job) = self.backend.running() {
            shell = shell.lamp(
                format!("RUNNING {}", job.label.split(" //").next().unwrap_or("")),
                pal.warn,
                true,
            );
        }
        if self.system.tray_running {
            shell = shell.lamp("TRAY", pal.accent, false);
        }
        if let Some((text, busy)) = self.agent.lamp() {
            shell = shell.lamp(text, if busy { pal.warn } else { pal.accent }, busy);
        }

        let log_open = self.settings.log_open;
        let mut log_resized = false;
        let mut log_toggled = false;
        let out = shell.show_full(ui, |ui| {
            let c = ui.max_rect();
            let top = c.top() + 10.0;
            let log_max = c.height() - BODY_MIN;
            self.log_h = self.log_h.clamp(LOG_MIN, log_max.max(LOG_MIN));
            let log_h = if log_open { self.log_h } else { LOG_CLOSED };
            let log_rect = Rect::from_min_max(pos2(c.left(), c.bottom() - log_h), c.max);
            let body_bottom = log_rect.top() - GAP - 8.0;
            let left =
                Rect::from_min_max(pos2(c.left(), top), pos2(c.left() + LEFT_W, body_bottom));
            let right =
                Rect::from_min_max(pos2(c.right() - RIGHT_W, top), pos2(c.right(), body_bottom));
            let center = Rect::from_min_max(
                pos2(left.right() + GAP, top),
                pos2(right.left() - GAP, body_bottom),
            );
            let views = Rect::from_min_size(left.min, vec2(left.width(), VIEWS_H));
            let system =
                Rect::from_min_max(pos2(left.left(), views.bottom() + GAP + 8.0), left.max);
            let toolbar = Rect::from_min_size(
                pos2(center.left(), center.top() - 8.0),
                vec2(center.width(), TOOLBAR_H),
            );
            let listing =
                Rect::from_min_max(pos2(center.left(), toolbar.bottom() + 12.0), center.max);

            self.ui_views(ui, views, &mut actions);
            self.ui_system(ui, system, &mut actions);
            self.ui_toolbar(ui, toolbar, &mut actions);
            self.ui_listing(ui, listing, &mut actions);
            self.ui_inspector(ui, right, &mut actions);
            if log_open {
                let strip = Rect::from_min_max(
                    pos2(c.left(), body_bottom),
                    pos2(c.right(), log_rect.top()),
                );
                let resp = widgets::h_splitter(
                    ui,
                    strip,
                    "log",
                    &mut self.log_h,
                    LOG_MIN,
                    log_max,
                    "LOG HEIGHT",
                );
                log_resized = resp.drag_stopped();
            }
            log_toggled = self.ui_log(ui, log_rect, log_open);
        });
        self.agent.paint(&ctx);
        if out.settings_clicked {
            actions.push(Action::OpenSettings);
        }
        if log_resized {
            self.settings.log_height = Some(self.log_h.round());
            self.save_settings(t);
        }
        if log_toggled {
            self.settings.log_open = !log_open;
            self.save_settings(t);
        }
        self.ui_dialog(&ctx, &mut actions);
        for a in actions {
            self.apply(&ctx, a, t);
        }
        if self.dirty {
            self.rebuild_rows();
        }
        self.settings_win
            .set_agent_status(&ctx, &self.agent.status_line());
        if self.settings_win.show(&ctx, &mut self.settings, APP_NAME) {
            self.settings_changed(t);
        }
    }
}

impl PkgApp {
    /// `FUIDE_DEV_DIALOG=install|remove|upgrade|run-upgrade|error|success`.
    fn dev_dialog_state(&mut self, kind: &str, actions: &mut Vec<Action>) {
        actions.push(Action::Select(Some(0)));
        let name = self
            .packages
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        match kind {
            "install" => actions.push(Action::Install(name, false)),
            "remove" => actions.push(Action::Remove(name)),
            "upgrade" => actions.push(Action::UpgradeAll),
            "run-upgrade" => {
                actions.push(Action::UpgradeAll);
                actions.push(Action::ConfirmDialog);
            }
            "error" => self
                .notice_queue
                .push_back((false, "UPGRADE // ALL".into())),
            "success" => self.notice_queue.push_back((true, "UPGRADE // ALL".into())),
            _ => {}
        }
    }

    fn ui_views(&self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        Panel::new("Views").show_rect(ui, rect, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            widgets::section_label(ui, "Packages");
            for (i, v) in View::ALL.into_iter().enumerate() {
                let count = self.count(v);
                let label = format!("{}  {count}", v.label());
                let resp = widgets::nav_tab(ui, &label, self.view == v);
                let r = resp.rect;
                ui.painter().text(
                    pos2(r.right() - 10.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{}{}", cmd_glyph(), i + 1),
                    mono(ts.small),
                    pal.text_dim,
                );
                if (v == View::Updates || v == View::Orphans) && count > 0 {
                    ui.painter()
                        .circle_filled(pos2(r.right() - 64.0, r.center().y), 3.0, pal.warn);
                }
                if resp.clicked() {
                    actions.push(Action::SetView(v));
                }
            }
        });
    }

    fn ui_system(&self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        let total = self.packages.len().max(1);
        let outdated = self.outdated_count();
        let current = 1.0 - outdated.min(total) as f32 / total as f32;
        let color = if self.system.reboot_required {
            pal.danger
        } else if outdated == 0 {
            pal.ok
        } else if outdated < 20 {
            pal.warn
        } else {
            pal.danger
        };
        let busy = self.backend.running().is_some();
        let size_total: f64 = self
            .packages
            .iter()
            .filter_map(|p| p.installed_size_kib)
            .sum();
        let orphans = self.packages.iter().filter(|p| p.orphan).count();
        Panel::new("System")
            .padding(12.0, 14.0)
            .show_rect(ui, rect, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    widgets::arc_gauge(ui, 30.0, current, "current", color);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        widgets::readout(ui, "packages", &self.packages.len().to_string(), None);
                        widgets::readout(
                            ui,
                            "explicit",
                            &self.count(View::Explicit).to_string(),
                            None,
                        );
                        widgets::readout(ui, "aur", &self.count(View::Foreign).to_string(), None);
                        widgets::readout(
                            ui,
                            "outdated",
                            &outdated.to_string(),
                            Some(if outdated > 0 { pal.warn } else { pal.ok }),
                        );
                    });
                });
                widgets::rule(ui);
                widgets::readout(ui, "installed", &archpkg::fmt_kib(size_total), None);
                widgets::readout(
                    ui,
                    "cache",
                    &format!(
                        "{}{}",
                        self.system
                            .cache_kib
                            .map(|k| archpkg::fmt_kib(k).trim().to_string())
                            .unwrap_or_else(|| "--".into()),
                        self.system
                            .cache_candidates
                            .filter(|c| *c > 0)
                            .map(|c| format!(" :: {c} OLD"))
                            .unwrap_or_default()
                    ),
                    None,
                );
                widgets::readout(
                    ui,
                    "orphans",
                    &orphans.to_string(),
                    Some(if orphans > 0 { pal.warn } else { pal.ok }),
                );
                widgets::readout(
                    ui,
                    "db synced",
                    &self
                        .system
                        .last_sync
                        .map(|t| archpkg::fmt_ago(t).to_uppercase())
                        .unwrap_or_else(|| "--".into()),
                    None,
                );
                widgets::readout(
                    ui,
                    "checked",
                    &self
                        .checked_at
                        .map(|t| archpkg::fmt_ago(t).to_uppercase())
                        .unwrap_or_else(|| "NEVER".into()),
                    Some(if self.check_error.is_some() {
                        pal.danger
                    } else {
                        pal.text
                    }),
                );
                widgets::readout(
                    ui,
                    "kernel",
                    if self.system.kernel.is_empty() {
                        "--"
                    } else {
                        &self.system.kernel
                    },
                    None,
                );
                widgets::readout(
                    ui,
                    "aur helper",
                    self.system.aur_helper.as_deref().unwrap_or("NONE"),
                    None,
                );
                widgets::readout(
                    ui,
                    "privilege",
                    self.system
                        .privilege
                        .as_deref()
                        .map(|p| {
                            std::path::Path::new(p)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(p)
                        })
                        .unwrap_or("NONE"),
                    Some(if self.system.privilege.is_some() {
                        pal.text
                    } else {
                        pal.danger
                    }),
                );
                widgets::readout(
                    ui,
                    "tray",
                    if self.system.tray_running {
                        "RUNNING"
                    } else {
                        "OFF"
                    },
                    None,
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::button(
                        ui,
                        vec2(104.0, ts.row),
                        "CLEAN CACHE",
                        !busy && self.system.cache_kib.is_some(),
                    )
                    .clicked()
                    {
                        actions.push(Action::CleanCache);
                    }
                    if widgets::button_colored(
                        ui,
                        vec2(104.0, ts.row),
                        &format!("ORPHANS  {orphans}"),
                        !busy && orphans > 0,
                        pal.warn,
                    )
                    .clicked()
                    {
                        actions.push(Action::RemoveOrphans);
                    }
                });
                let (fr, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), ts.small + 6.0),
                    Sense::hover(),
                );
                ui.painter().with_clip_rect(fr).text(
                    pos2(fr.left(), fr.center().y),
                    Align2::LEFT_CENTER,
                    format!(
                        "{}R REFRESH :: {}U CHECK :: {}1..6 VIEWS",
                        cmd_glyph(),
                        cmd_glyph(),
                        cmd_glyph()
                    ),
                    mono(ts.small),
                    pal.text_dim,
                );
            });
    }

    fn ui_toolbar(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        let busy = self.backend.running().is_some();
        const FILTER_W: f32 = 180.0;
        let left_rect = Rect::from_min_max(
            rect.min,
            pos2(rect.right() - FILTER_W - 12.0, rect.bottom()),
        );
        let right_rect = Rect::from_min_max(pos2(rect.right() - FILTER_W, rect.top()), rect.max);
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("toolbar-left")
                .max_rect(left_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        {
            let ui = &mut left;
            ui.spacing_mut().item_spacing.x = 6.0;
            if widgets::icon_button(
                ui,
                vec2(32.0, ts.row),
                widgets::Icon::Refresh,
                !self.backend.fetching(),
            )
            .on_hover_text("Re-read the installed packages (Ctrl+R)")
            .clicked()
            {
                actions.push(Action::Refresh);
            }
            if widgets::button(ui, vec2(84.0, ts.row), "CHECK", !self.backend.checking())
                .on_hover_text("checkupdates + AUR helper -Qua (network, no root)")
                .clicked()
            {
                actions.push(Action::Check);
            }
            let n = self.outdated_count();
            if widgets::button_colored(
                ui,
                vec2(150.0, ts.row),
                &format!("UPGRADE ALL  {n}"),
                !busy && n > 0,
                pal.warn,
            )
            .on_hover_text("Full system upgrade (-Syu)")
            .clicked()
            {
                actions.push(Action::UpgradeAll);
            }
            if self.view == View::Search {
                ui.add_space(10.0);
                let resp =
                    widgets::text_input(ui, 240.0, &mut self.search_query, "search repos and aur");
                if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                    resp.request_focus();
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    actions.push(Action::Search);
                }
                if widgets::button(
                    ui,
                    vec2(84.0, ts.row),
                    "SEARCH",
                    self.backend.searching().is_none(),
                )
                .clicked()
                {
                    actions.push(Action::Search);
                }
            } else {
                ui.add_space(10.0);
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ts.row), Sense::hover());
                let text = match self.view {
                    View::Installed => "ALL INSTALLED PACKAGES".to_string(),
                    View::Explicit => "PACKAGES YOU ASKED FOR (NOT DEPENDENCIES)".to_string(),
                    View::Updates => match &self.check_error {
                        Some(e) => format!("LAST CHECK FAILED :: {e}"),
                        None => match self.checked_at {
                            Some(t) => format!("CHECKED {}", archpkg::fmt_ago(t).to_uppercase()),
                            None => "NOT CHECKED YET".to_string(),
                        },
                    },
                    View::Foreign => "PACKAGES NOT FROM THE REPOSITORIES (AUR / LOCAL)".to_string(),
                    View::Orphans => "DEPENDENCIES NOTHING REQUIRES ANY MORE".to_string(),
                    View::Search => String::new(),
                };
                ui.painter().with_clip_rect(r).text(
                    pos2(r.left(), r.center().y),
                    Align2::LEFT_CENTER,
                    text,
                    mono(ts.label),
                    pal.text_dim,
                );
            }
        }
        let want_focus =
            self.view != View::Search && ui.input(|i| i.modifiers.command && i.key_pressed(Key::F));
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("toolbar-right")
                .max_rect(right_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        {
            let ui = &mut right;
            let resp = widgets::text_input(ui, FILTER_W, &mut self.filter, "filter");
            if resp.changed() {
                self.dirty = true;
            }
            if want_focus {
                resp.request_focus();
            }
            if (resp.has_focus() || resp.lost_focus()) && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.filter.clear();
                self.dirty = true;
                resp.surrender_focus();
            }
        }
    }

    fn ui_listing(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let pal = palette(ui.ctx());
        let title = match self.view {
            View::Search => "Search results",
            v => v.label(),
        };
        let tag = format!("{} items", self.rows.len());
        let columns = [
            Column::new("NAME", Width::Flex),
            Column::new("VERSION", Width::Chars(15.0)),
            Column::new("LATEST", Width::Chars(15.0)),
            Column::new("REPO", Width::Chars(17.0)),
            Column::new("STATUS", Width::Chars(10.0)),
            Column::new("SIZE", Width::Chars(10.0)).right(),
        ];
        let src: &[Package] = if self.view == View::Search {
            &self.search_results
        } else {
            &self.packages
        };
        let rows = &self.rows;
        let mut state = std::mem::take(&mut self.table);
        let resp = Panel::new(title)
            .tag(tag, pal.text_dim)
            .padding(8.0, 14.0)
            .show_rect(ui, rect, |ui| {
                table::table(
                    ui,
                    "packages",
                    &columns,
                    rows.len(),
                    &mut state,
                    |row, col| {
                        let p = &src[rows[row]];
                        match col {
                            0 => Cell::text(&p.name).color(if p.installed {
                                pal.text
                            } else {
                                pal.text_dim
                            }),
                            1 => Cell::dim(&p.version),
                            2 => Cell::dim(p.latest.as_deref().unwrap_or("")).color(pal.warn),
                            3 => Cell::tag(&p.repo).color(if p.is_aur() {
                                pal.accent_dim
                            } else {
                                pal.text_dim
                            }),
                            4 => {
                                let st = status_of(p);
                                Cell::tag(st.tag()).color(match st {
                                    Status::Outdated | Status::Orphan => pal.warn,
                                    Status::Explicit => pal.ok.gamma_multiply(0.8),
                                    Status::Dependency | Status::Available => pal.text_dim,
                                })
                            }
                            _ => Cell::dim(
                                p.installed_size_kib
                                    .map(archpkg::fmt_kib)
                                    .unwrap_or_default(),
                            ),
                        }
                    },
                )
            });
        self.table = state;
        if resp.sort_changed {
            self.dirty = true;
        }
        if resp.clicked.is_some() {
            actions.push(Action::Select(self.table.selected));
        }
        if let Some(r) = resp.double_clicked {
            if let Some(p) = rows.get(r).and_then(|&i| src.get(i)) {
                if !p.url.is_empty() {
                    actions.push(Action::Homepage(p.url.clone()));
                }
            }
        }
    }

    fn ui_inspector(&self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        let busy = self.backend.running().is_some();
        let sel = self.selected_package();
        let tag = sel.map(|p| p.repo.as_str()).unwrap_or("none").to_string();
        Panel::new("Package").tag(tag, pal.text_dim).show_rect(ui, rect, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let Some(p) = sel else {
                ui.add_space(4.0);
                ui.label(RichText::new("SELECT A PACKAGE").font(mono(ts.label)).color(pal.text_dim));
                return;
            };
            egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(RichText::new(&p.name).font(mono(ts.data + 3.0)).color(pal.accent)).wrap());
                if !p.desc.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&p.desc).font(mono(ts.label)).color(pal.text)).wrap());
                }
                ui.add_space(6.0);
                widgets::rule(ui);
                let st = status_of(p);
                widgets::readout(
                    ui,
                    "status",
                    st.tag(),
                    Some(match st {
                        Status::Outdated | Status::Orphan => pal.warn,
                        Status::Explicit => pal.ok,
                        _ => pal.text_dim,
                    }),
                );
                widgets::readout(ui, "version", &p.version, None);
                if let Some(l) = &p.latest {
                    widgets::readout(ui, "latest", l, Some(pal.warn));
                }
                widgets::readout(ui, "repo", &p.repo, None);
                if let Some(s) = p.installed_size_kib {
                    widgets::readout(ui, "installed size", archpkg::fmt_kib(s).trim(), None);
                }
                if let Some(s) = p.download_size_kib {
                    widgets::readout(ui, "download", archpkg::fmt_kib(s).trim(), None);
                }
                if !p.install_date.is_empty() {
                    widgets::readout(ui, "installed on", &p.install_date, None);
                }
                if !p.build_date.is_empty() {
                    widgets::readout(ui, "built", &p.build_date, None);
                }
                if !p.licenses.is_empty() {
                    widgets::readout(ui, "license", &p.licenses.join(", "), None);
                }
                if !p.groups.is_empty() {
                    widgets::readout(ui, "groups", &p.groups.join(", "), None);
                }
                if !p.packager.is_empty() {
                    widgets::readout(ui, "packager", p.packager.split('<').next().unwrap_or("").trim(), None);
                }
                if !p.maintainer.is_empty() {
                    widgets::readout(ui, "maintainer", &p.maintainer, None);
                }
                if let Some(v) = p.votes {
                    widgets::readout(ui, "votes", &format!("{v} :: {:.2}", p.popularity.unwrap_or(0.0)), None);
                }
                if p.out_of_date {
                    widgets::readout(ui, "aur flag", "OUT OF DATE", Some(pal.danger));
                }
                if !p.validated.is_empty() {
                    widgets::readout(ui, "validated", &p.validated, None);
                }
                for (title, list, color) in [
                    ("DEPENDS", &p.depends, pal.text_dim),
                    ("REQUIRED BY", &p.required_by, pal.text_dim),
                    ("OPTIONAL", &p.optdepends, pal.text_dim),
                    ("PROVIDES", &p.provides, pal.text_dim),
                    ("CONFLICTS", &p.conflicts, pal.warn),
                ] {
                    if list.is_empty() {
                        continue;
                    }
                    ui.add_space(6.0);
                    let (lr, _) = ui.allocate_exact_size(vec2(ui.available_width(), ts.heading + 4.0), Sense::hover());
                    fuide::display_text(
                        ui.painter(),
                        pos2(lr.left(), lr.center().y),
                        Align2::LEFT_CENTER,
                        format!("{title} {}", list.len()),
                        ts.heading,
                        color,
                    );
                    let shown: Vec<&str> = list.iter().take(40).map(String::as_str).collect();
                    let mut text = shown.join("  ");
                    if list.len() > 40 {
                        text.push_str(&format!("  … +{}", list.len() - 40));
                    }
                    ui.add(egui::Label::new(RichText::new(text).font(mono(ts.small)).color(pal.text_dim)).wrap());
                }
                if !p.url.is_empty() {
                    ui.add_space(6.0);
                    widgets::rule(ui);
                    ui.add(egui::Label::new(RichText::new(&p.url).font(mono(ts.small)).color(pal.text_dim)).wrap());
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::button(ui, vec2(96.0, ts.row), "HOMEPAGE", !p.url.is_empty()).clicked() {
                        actions.push(Action::Homepage(p.url.clone()));
                    }
                    if widgets::button(ui, vec2(72.0, ts.row), "COPY", true).clicked() {
                        actions.push(Action::CopyName(p.name.clone()));
                    }
                    if p.is_aur() {
                        if widgets::button(ui, vec2(96.0, ts.row), "PKGBUILD", true)
                            .on_hover_text("Open the PKGBUILD on aur.archlinux.org")
                            .clicked()
                        {
                            actions.push(Action::Pkgbuild(p.name.clone()));
                        }
                    } else if p.installed {
                        let (label, to) = if p.reason == Reason::Explicit { ("AS DEP", false) } else { ("EXPLICIT", true) };
                        if widgets::button(ui, vec2(96.0, ts.row), label, !busy)
                            .on_hover_text("pacman -D --asexplicit / --asdeps")
                            .clicked()
                        {
                            actions.push(Action::MarkExplicit(p.name.clone(), to));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if p.installed {
                        if p.is_aur() && p.outdated() {
                            if widgets::button_colored(ui, vec2(96.0, ts.row), "UPGRADE", !busy, pal.warn).clicked() {
                                actions.push(Action::UpgradeAur(p.name.clone()));
                            }
                        } else if p.outdated()
                            && widgets::button_colored(ui, vec2(140.0, ts.row), "UPGRADE ALL", !busy, pal.warn)
                                .on_hover_text("Repo packages are upgraded together (-Syu); partial upgrades are unsupported")
                                .clicked()
                        {
                            actions.push(Action::UpgradeAll);
                        }
                        if widgets::button_colored(ui, vec2(96.0, ts.row), "REMOVE", !busy, pal.danger).clicked() {
                            actions.push(Action::Remove(p.name.clone()));
                        }
                    } else if widgets::button(ui, vec2(96.0, ts.row), "INSTALL", !busy).clicked() {
                        actions.push(Action::Install(p.name.clone(), p.is_aur()));
                    }
                });
            });
        });
    }

    /// Returns `true` when the title chip was clicked (the caller flips `Settings::log_open`).
    fn ui_log(&self, ui: &mut Ui, rect: Rect, open: bool) -> bool {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        let tag = match self.backend.running() {
            Some(j) => j.command_line(),
            None => format!("{} lines", self.log.len()),
        };
        let (_, toggled) = Panel::new("Event log")
            .tag(
                tag,
                if self.backend.running().is_some() {
                    pal.warn
                } else {
                    pal.text_dim
                },
            )
            .padding(8.0, 12.0)
            .show_collapsible_rect(ui, rect, open, |ui| {
                if !open {
                    return;
                }
                let lines: Vec<LogLine> = self
                    .log
                    .iter()
                    .map(|l| LogLine {
                        time: l.time.clone(),
                        text: l.text.clone(),
                        color: match l.level {
                            Level::Info => pal.text,
                            Level::Ok => pal.ok,
                            Level::Warn => pal.warn,
                            Level::Danger => pal.danger,
                        },
                    })
                    .collect();
                widgets::log_feed(
                    ui,
                    &lines,
                    pal.text_dim,
                    ts.label,
                    widgets::LogOrder::NewestFirst,
                );
            });
        toggled
    }

    fn ui_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let pal = palette(ctx);
        let ts = type_scale(ctx);
        let Some(OpenDialog { state, closing }) = &mut self.dialog else {
            return;
        };
        let open = !*closing;
        let enter = open && ctx.input(|i| i.key_pressed(Key::Enter));
        let finished;
        match state {
            DialogState::Confirm(c) => {
                let color = if c.danger { pal.danger } else { pal.warn };
                let program = std::path::Path::new(&c.job.program)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| c.job.program.clone());
                let resp = Dialog::new(&c.title)
                    .tag(program, pal.text_dim)
                    .outline(color)
                    .width(500.0)
                    .show(ctx, open, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add(egui::Label::new(RichText::new(&c.line).font(mono(ts.data + 2.0)).color(pal.accent)).wrap());
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("$ {}", c.job.command_line()))
                                    .font(mono(ts.label))
                                    .color(pal.text_dim),
                            )
                            .wrap(),
                        );
                        ui.add_space(6.0);
                        widgets::rule(ui);
                        ui.add(egui::Label::new(RichText::new(&c.note).font(mono(ts.label)).color(color)).wrap());
                        note(ui, "THE DESKTOP ASKS FOR YOUR PASSWORD (POLKIT) :: OUTPUT GOES TO THE EVENT LOG", pal.text_dim);
                        ui.add_space(8.0);
                        fuide::dialog::button_row(ui, &[("CANCEL", pal.text_dim, true), (&c.verb, color, true)])
                    });
                finished = resp.finished;
                let clicked = resp.inner.flatten();
                if !open {
                } else if resp.should_close || clicked == Some(0) {
                    actions.push(Action::CloseDialog);
                } else if enter || clicked == Some(1) {
                    actions.push(Action::ConfirmDialog);
                }
            }
            DialogState::Notice { success, line } => {
                let (word, color) = if *success {
                    ("Success", pal.ok)
                } else {
                    ("Error", pal.danger)
                };
                let resp =
                    fuide::dialog::alert(ctx, open, word, line, "details :: event log", color);
                finished = resp.finished;
                if open && (resp.should_close || resp.inner == Some(true)) {
                    actions.push(Action::CloseDialog);
                }
            }
        }
        if finished {
            self.dialog = None;
        }
    }
}

fn note(ui: &mut Ui, text: &str, color: egui::Color32) {
    let ts = type_scale(ui.ctx());
    let (nr, _) = ui.allocate_exact_size(vec2(ui.available_width(), ts.row), Sense::hover());
    ui.painter().with_clip_rect(nr).text(
        pos2(nr.left() + 2.0, nr.center().y),
        Align2::LEFT_CENTER,
        text,
        mono(ts.small),
        color,
    );
}

#[cfg(test)]
mod e2e;
#[cfg(test)]
mod tests;
