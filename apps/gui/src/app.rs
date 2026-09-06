//! FUIDE Arch-Update — pacman / AUR package manager with a tactical-console look.
//!
//! Left: views + system readout. Centre: package table above the console pacman runs in.
//! Right: package inspector + event log. Root commands go through the console (a pty) so
//! `sudo` and pacman's questions become dialogs.

use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{pos2, vec2, Align2, Key, Rect, RichText, Sense, Stroke, Ui};
use fuide::table::{self, Cell, Column, TableState, Width};
use fuide::widgets::{self, LogLine};
use fuide::{
    mono, palette, theme, type_scale, Dialog, PaletteKind, Panel, Settings, SettingsWindow, Shell,
};

use archpkg::pacman::{self, Package, Reason, SearchHit};
use archpkg::Upgrade;

use crate::backend::{self, Backend, Msg, SystemInfo};
use crate::prompt::{self, Prompt};
use crate::pty::{self, Runner};
use crate::term::{Hue, Terminal};

pub const APP_ID: &str = "arch-update";
pub const APP_NAME: &str = "FUIDE Arch-Update";

const LEFT_W: f32 = 236.0;
const RIGHT_W: f32 = 340.0;
const GAP: f32 = 14.0;
const TOOLBAR_H: f32 = 32.0;
const CONSOLE_H: f32 = 220.0;
const CONSOLE_MIN: f32 = 90.0;
const CONSOLE_CLOSED: f32 = 26.0;
const TABLE_MIN: f32 = 200.0;
const VIEWS_H: f32 = 268.0;
const EVENTS_H: f32 = 170.0;
const CONSOLE_VISIBLE: usize = 800;
const PROMPT_QUIET_SECS: f64 = 0.15;

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

/// A root / helper command to run in the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
}

impl Job {
    pub fn command_line(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
            .trim_end()
            .to_string()
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptKey {
    pub row: usize,
    pub text: String,
}

pub enum DialogState {
    Confirm(Confirm),
    Prompt {
        prompt: Prompt,
        key: PromptKey,
        input: String,
        selected: Vec<bool>,
    },
    Abort,
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
    CopyName(String),
    Run(Job),
    Abort,
    ConfirmAbort,
    SendLine(String),
    Answer(String),
    Dismiss,
    CloseDialog,
    ConfirmDialog,
    ToggleEnglish,
    ClearConsole,
    OpenSettings,
}

/// App options that are not theme settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Run console commands under `LC_ALL=C.UTF-8` so their prompts are recognised.
    pub english: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { english: true }
    }
}

impl Options {
    pub fn path() -> Option<PathBuf> {
        let p = Settings::path(APP_ID)?;
        Some(p.with_file_name(format!("{APP_ID}.app.conf")))
    }
    pub fn load_from(path: &std::path::Path) -> Self {
        let mut o = Self::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == "english" {
                        o.english = v.trim() != "false";
                    }
                }
            }
        }
        o
    }
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            format!("# fuide-arch-update options\nenglish={}\n", self.english),
        )
    }
}

pub struct PkgApp {
    pub backend: Backend,
    pub runner: Runner,
    pub term: Terminal,
    pub packages: Vec<Package>,
    pub updates: Vec<Upgrade>,
    pub checked_at: Option<u64>,
    pub check_error: Option<String>,
    pub search_results: Vec<Package>,
    pub search_query: String,
    pub system: SystemInfo,
    pub view: View,
    pub rows: Vec<usize>,
    pub table: TableState,
    pub dirty: bool,
    pub filter: String,
    pub log: Vec<Event>,
    pub dialog: Option<OpenDialog>,
    notice_queue: VecDeque<(bool, String)>,
    answered: Option<PromptKey>,
    last_output_at: f64,
    console_input: String,
    console_focus_pending: bool,
    fetch_ms: f32,
    /// Names whose `-Si` details were requested (search results).
    details_requested: std::collections::HashSet<String>,
    pub options: Options,
    options_path: Option<PathBuf>,
    start: StartUp,
    devshot: fuide::devshot::DevShot,
    agent: fuide::Agent,
    dev_dialog: Option<String>,
    dev_search: Option<String>,
    dev_frame: u32,
    dev_close_frame: Option<u32>,
    pub settings: Settings,
    settings_win: SettingsWindow,
    settings_path: Option<PathBuf>,
    console_h: f32,
}

impl PkgApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: StartUp) -> Self {
        let settings = Settings::load(APP_ID).unwrap_or_else(|| Settings::new(PaletteKind::Cyan));
        let options = Options::path()
            .map(|p| Options::load_from(&p))
            .unwrap_or_default();
        let mut app = Self::with_context(&cc.egui_ctx, settings, options);
        app.settings_path = Settings::path(APP_ID);
        app.options_path = Options::path();
        app.start = start;
        app.load_saved_check();
        app.backend.fetch_inventory(cc.egui_ctx.clone());
        app.backend.fetch_system(cc.egui_ctx.clone());
        // a live check at start (network); the tray does the same on its timer
        app.backend.check(cc.egui_ctx.clone());
        app
    }

    pub fn with_context(ctx: &egui::Context, settings: Settings, options: Options) -> Self {
        theme::install(ctx, settings.palette.palette(), cjk_fallback());
        settings.apply(ctx);
        let mut app = Self {
            backend: Backend::new(),
            runner: Runner::new(),
            term: Terminal::new(),
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
            answered: None,
            last_output_at: 0.0,
            console_input: String::new(),
            console_focus_pending: false,
            fetch_ms: 0.0,
            details_requested: Default::default(),
            options,
            options_path: None,
            start: StartUp::default(),
            devshot: fuide::devshot::DevShot::from_env(),
            agent: fuide::Agent::new(APP_ID, APP_NAME),
            dev_dialog: std::env::var("FUIDE_DEV_DIALOG").ok(),
            dev_search: std::env::var("FUIDE_DEV_SEARCH").ok(),
            dev_frame: 0,
            dev_close_frame: std::env::var("FUIDE_DEV_DIALOG_CLOSE")
                .ok()
                .and_then(|v| v.parse().ok()),
            console_h: settings.log_height.unwrap_or(CONSOLE_H),
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
        if self.log.len() > 2000 {
            self.log.drain(..500);
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

    fn count(&self, v: View) -> usize {
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

    /// While a confirmation is open and the agent may not confirm, its verb is human-only;
    /// passwords always are.
    fn agent_blocked(&self) -> Vec<String> {
        let mut v = Vec::new();
        match &self.dialog {
            Some(OpenDialog {
                state: DialogState::Confirm(c),
                closing: false,
            }) if !self.settings.agent_confirm => v.push(c.verb.clone()),
            Some(OpenDialog {
                state: DialogState::Prompt { prompt, .. },
                closing: false,
            }) => {
                if matches!(prompt, Prompt::Password { .. })
                    || (prompt.consequential() && !self.settings.agent_confirm)
                {
                    v.push(prompt.verb().to_string());
                }
            }
            Some(OpenDialog {
                state: DialogState::Abort,
                closing: false,
            }) if !self.settings.agent_confirm => v.push("ABORT".into()),
            _ => {}
        }
        v
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
        if let Some(j) = self.runner.job() {
            let _ = writeln!(s, "running: {}", j.label);
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
                DialogState::Prompt { prompt, .. } => {
                    let _ = writeln!(
                        s,
                        "dialog: PROMPT {} :: {} :: {}",
                        prompt.title(),
                        prompt.text(),
                        prompt.verb()
                    );
                }
                DialogState::Abort => s.push_str("dialog: ABORT confirmation\n"),
                DialogState::Notice { success, line } => {
                    let _ = writeln!(
                        s,
                        "dialog: {} :: {line} :: press ACKNOWLEDGE",
                        if *success { "SUCCESS" } else { "ERROR" }
                    );
                }
            }
        }
        s.push_str("console (latest last):\n");
        let n = self.term.len();
        for i in n.saturating_sub(8)..n {
            let _ = writeln!(s, "  {}", self.term.text(i));
        }
        s.push_str("log (latest last):\n");
        let skip = self.log.len().saturating_sub(5);
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
                            self.after_inventory(ctx, t);
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
            }
        }
        for msg in self.runner.poll() {
            match msg {
                pty::Msg::Output(bytes) => {
                    self.term.feed(&bytes);
                    self.last_output_at = t;
                }
                pty::Msg::Exit {
                    label,
                    code,
                    signal,
                    elapsed_secs,
                    ..
                } => {
                    self.answered = None;
                    let time = format!("{elapsed_secs:.1} s");
                    match (code, signal) {
                        (Some(0), _) => {
                            self.succeed(t, label.to_uppercase(), &format!("done in {time}"))
                        }
                        (_, Some(sig)) => self.push_log(
                            t,
                            format!("{label} :: stopped by signal {sig}"),
                            Level::Warn,
                        ),
                        (Some(c), _) => {
                            self.fail(t, label.to_uppercase(), &format!("exit code {c}"))
                        }
                        (None, None) => self.fail(t, label.to_uppercase(), "could not start"),
                    }
                    // the package database changed: re-read everything and re-check (the
                    // state file tells the tray)
                    self.backend.fetch_inventory(ctx.clone());
                    self.backend.fetch_system(ctx.clone());
                    self.backend.check(ctx.clone());
                }
            }
        }
    }

    /// Start-up requests that need the inventory: `--select`, `--upgrade`.
    fn after_inventory(&mut self, _ctx: &egui::Context, _t: f64) {
        if let Some(name) = self.start.select.take() {
            self.view = View::Updates;
            self.filter.clear();
            self.rebuild_rows_for_select(&name);
        }
        if self.start.upgrade {
            self.start.upgrade = false;
            self.open_upgrade_all();
        }
    }

    fn rebuild_rows_for_select(&mut self, name: &str) {
        self.rebuild_rows();
        let pos = self
            .rows
            .iter()
            .position(|&i| self.packages[i].name == name);
        if pos.is_none() {
            self.view = View::Installed;
            self.rebuild_rows();
        }
        let pos = pos.or_else(|| {
            self.rows
                .iter()
                .position(|&i| self.packages[i].name == name)
        });
        self.table.selected = pos;
        self.table.scroll_to_selected = true;
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

    pub fn check_prompt(&mut self, now: f64) {
        if !self.runner.running() || self.dialog.is_some() {
            return;
        }
        let text = self.term.cursor_text();
        if text.trim().is_empty() || now - self.last_output_at < PROMPT_QUIET_SECS {
            return;
        }
        let key = PromptKey {
            row: self.term.dropped + self.term.cursor_row(),
            text: text.clone(),
        };
        if self.answered.as_ref() == Some(&key) {
            return;
        }
        let above = self.term.tail(40);
        let above = &above[..above.len().saturating_sub(1)];
        let Some(prompt) = prompt::detect(above, &text) else {
            return;
        };
        let n = if let Prompt::Select { items, .. } = &prompt {
            items.len()
        } else {
            0
        };
        let input = if let Prompt::Input { default, .. } = &prompt {
            default.clone()
        } else {
            String::new()
        };
        self.push_log(
            now,
            format!("prompt :: {}", prompt::clean(&text)),
            Level::Warn,
        );
        self.dialog = Some(OpenDialog {
            state: DialogState::Prompt {
                prompt,
                key,
                input,
                selected: vec![false; n],
            },
            closing: false,
        });
    }

    fn confirm(&mut self, c: Confirm) {
        self.dialog = Some(OpenDialog {
            state: DialogState::Confirm(c),
            closing: false,
        });
    }

    /// `sudo pacman <args>` (or whatever privilege command is installed).
    fn root_job(&self, label: &str, args: &[&str]) -> Result<Job, String> {
        let su = pacman::privilege_cmd().ok_or_else(|| {
            "no privilege elevation command (sudo, sudo-rs, doas, run0)".to_string()
        })?;
        let mut full = vec![pacman::pacman_bin().display().to_string()];
        full.extend(args.iter().map(|s| s.to_string()));
        Ok(Job {
            label: label.into(),
            program: su,
            args: full,
        })
    }

    fn helper_job(&self, label: &str, args: &[&str]) -> Result<Job, String> {
        let helper = pacman::aur_helper()
            .ok_or_else(|| "no AUR helper installed (yay, paru, pikaur)".to_string())?;
        Ok(Job {
            label: label.into(),
            program: helper.display().to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn open_upgrade_all(&mut self) {
        let n = self.updates.len();
        let aur = self.updates.iter().filter(|u| u.aur).count();
        let job = if aur > 0 || self.system.aur_helper.is_some() && pacman::aur_helper().is_some() {
            self.helper_job("upgrade // all", &["-Syu"])
        } else {
            self.root_job("upgrade // all", &["-Syu"])
        };
        match job {
            Ok(job) => self.confirm(Confirm {
                title: "Full upgrade".into(),
                line: format!("{n} packages ({} repo, {aur} aur)", n - aur),
                note: "SYNCS THE DATABASES AND UPGRADES EVERYTHING :: PARTIAL UPGRADES ARE NOT OFFERED".into(),
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
                if !self.system.checkupdates
                    && self.fetch_ms > 0.0
                    && !self.system.kernel.is_empty()
                {
                    self.fail(
                        t,
                        "CHECK // UPDATES",
                        "checkupdates not found (install pacman-contrib)",
                    );
                    return;
                }
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
                    self.root_job(&format!("install // {name}"), &["-S", "--needed", &name])
                };
                match job {
                    Ok(job) => self.confirm(Confirm {
                        title: "Install".into(),
                        line: name.clone(),
                        note: if aur {
                            "BUILT FROM THE AUR WITH THE HELPER :: REVIEW THE PKGBUILD WHEN ASKED"
                                .into()
                        } else {
                            "DEPENDENCIES ARE INSTALLED AS NEEDED".into()
                        },
                        verb: "INSTALL".into(),
                        danger: false,
                        job,
                    }),
                    Err(e) => self.fail(t, format!("INSTALL // {}", name.to_uppercase()), &e),
                }
            }
            Action::Remove(name) => match self
                .root_job(&format!("remove // {name}"), &["-Rns", &name])
            {
                Ok(job) => self.confirm(Confirm {
                    title: "Remove".into(),
                    line: name.clone(),
                    note: "REMOVES THE PACKAGE, ITS CONFIG AND UNNEEDED DEPENDENCIES (-Rns)".into(),
                    verb: "REMOVE".into(),
                    danger: true,
                    job,
                }),
                Err(e) => self.fail(t, format!("REMOVE // {}", name.to_uppercase()), &e),
            },
            Action::UpgradeAur(name) => {
                match self.helper_job(&format!("upgrade // {name}"), &["-S", &name]) {
                    Ok(job) => self.confirm(Confirm {
                        title: "Upgrade".into(),
                        line: name.clone(),
                        note: "REBUILT FROM THE AUR WITH THE HELPER".into(),
                        verb: "UPGRADE".into(),
                        danger: false,
                        job,
                    }),
                    Err(e) => self.fail(t, format!("UPGRADE // {}", name.to_uppercase()), &e),
                }
            }
            Action::MarkExplicit(name, explicit) => {
                let flag = if explicit { "--asexplicit" } else { "--asdeps" };
                match self.root_job(&format!("mark // {name}"), &["-D", flag, &name]) {
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
                let mut args = vec!["-Rns"];
                args.extend(names.iter().map(String::as_str));
                match self.root_job("remove // orphans", &args) {
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
                let su = match pacman::privilege_cmd() {
                    Some(s) => s,
                    None => {
                        self.fail(t, "CLEAN // CACHE", "no privilege elevation command");
                        return;
                    }
                };
                let job = Job {
                    label: "clean // cache".into(),
                    program: su,
                    args: vec![pacman::paccache_bin().display().to_string(), "-rk2".into()],
                };
                self.confirm(Confirm {
                    title: "Clean cache".into(),
                    line: self
                        .system
                        .cache_kib
                        .map(|k| format!("{} in /var/cache/pacman/pkg", archpkg::fmt_kib(k).trim()))
                        .unwrap_or_else(|| "package cache".into()),
                    note: "KEEPS THE LAST 2 VERSIONS OF EACH PACKAGE (paccache -rk2)".into(),
                    verb: "CLEAN".into(),
                    danger: false,
                    job,
                });
            }
            Action::Homepage(url) => {
                self.push_log(t, format!("open // {url}"), Level::Info);
                if let Err(e) = open::that_detached(&url) {
                    self.fail(t, "OPEN // HOMEPAGE", &e.to_string());
                }
            }
            Action::CopyName(name) => {
                ctx.copy_text(name);
                self.push_log(t, "name copied to clipboard", Level::Info);
            }
            Action::Run(job) => {
                if self.runner.running() {
                    self.fail(
                        t,
                        job.label.to_uppercase(),
                        "another command is still running",
                    );
                    return;
                }
                self.answered = None;
                self.last_output_at = t;
                if !self.term.is_empty() {
                    self.term.feed(b"\r\n");
                }
                self.term
                    .feed(format!("\x1b[2m$ {}\x1b[0m\r\n", job.command_line()).as_bytes());
                if self.runner.run_program(
                    PathBuf::from(&job.program),
                    job.label.clone(),
                    job.args.clone(),
                    self.options.english,
                    ctx.clone(),
                ) {
                    self.push_log(t, format!("$ {}", job.command_line()), Level::Info);
                    self.settings.log_open = true;
                    self.console_focus_pending = true;
                }
            }
            Action::Abort => {
                if !self.runner.running() {
                    return;
                }
                self.dialog = Some(OpenDialog {
                    state: DialogState::Abort,
                    closing: false,
                });
            }
            Action::ConfirmAbort => {
                if let Some(d) = &mut self.dialog {
                    d.closing = true;
                }
                if self.runner.interrupt() {
                    self.push_log(t, "abort // ctrl+c sent", Level::Warn);
                } else {
                    self.runner.terminate();
                    self.push_log(t, "abort // sigterm sent", Level::Warn);
                }
            }
            Action::SendLine(text) => {
                if !self.runner.running() {
                    return;
                }
                if self.runner.send_line(&text) {
                    if let Some(d) = &mut self.dialog {
                        if let DialogState::Prompt { key, .. } = &d.state {
                            self.answered = Some(key.clone());
                        }
                        d.closing = true;
                    }
                    self.push_log(t, format!("console // sent {:?}", text), Level::Info);
                }
            }
            Action::Answer(text) => {
                let Some(OpenDialog {
                    state: DialogState::Prompt { key, prompt, .. },
                    closing,
                }) = &mut self.dialog
                else {
                    return;
                };
                if *closing {
                    return;
                }
                let secret = matches!(prompt, Prompt::Password { .. });
                let key = key.clone();
                *closing = true;
                if self.runner.send_line(&text) {
                    self.answered = Some(key);
                    if secret {
                        self.push_log(t, "answer // password sent", Level::Info);
                    } else {
                        self.push_log(t, format!("answer // {:?}", text), Level::Info);
                    }
                } else {
                    self.fail(t, "ANSWER // SEND", "the process is not reading");
                }
            }
            Action::Dismiss | Action::CloseDialog => {
                if let Some(d) = &mut self.dialog {
                    if let DialogState::Prompt { key, .. } = &d.state {
                        self.answered = Some(key.clone());
                        self.console_focus_pending = true;
                    }
                    d.closing = true;
                }
            }
            Action::ConfirmDialog => {
                let Some(d) = &self.dialog else { return };
                if d.closing {
                    return;
                }
                match &d.state {
                    DialogState::Confirm(c) => {
                        let job = c.job.clone();
                        if let Some(d) = &mut self.dialog {
                            d.closing = true;
                        }
                        self.apply(ctx, Action::Run(job), t);
                    }
                    DialogState::Abort => self.apply(ctx, Action::ConfirmAbort, t),
                    DialogState::Notice { .. } => self.apply(ctx, Action::CloseDialog, t),
                    DialogState::Prompt {
                        prompt,
                        input,
                        selected,
                        ..
                    } => {
                        let text = match prompt {
                            Prompt::YesNo { .. } => "y".to_string(),
                            Prompt::Continue { .. } => String::new(),
                            Prompt::Password { .. } | Prompt::Input { .. } => input.clone(),
                            Prompt::Select { .. } => selected
                                .iter()
                                .enumerate()
                                .filter(|(_, s)| **s)
                                .map(|(i, _)| (i + 1).to_string())
                                .collect::<Vec<_>>()
                                .join(" "),
                        };
                        self.apply(ctx, Action::Answer(text), t);
                    }
                }
            }
            Action::ToggleEnglish => {
                self.options.english = !self.options.english;
                self.push_log(
                    t,
                    format!(
                        "options // console locale {}",
                        if self.options.english {
                            "C.UTF-8 (english prompts)"
                        } else {
                            "system"
                        }
                    ),
                    Level::Warn,
                );
                if let Some(p) = self.options_path.clone() {
                    if let Err(e) = self.options.save_to(&p) {
                        self.push_log(t, format!("options // save failed: {e}"), Level::Danger);
                    }
                }
            }
            Action::ClearConsole => {
                if !self.runner.running() {
                    self.term.clear();
                }
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
            if cmd && i.key_pressed(Key::L) {
                actions.push(Action::ClearConsole);
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
        self.poll(&ctx, t);
        self.check_prompt(t);
        if self.runner.running()
            && !self.term.cursor_text().trim().is_empty()
            && self.dialog.is_none()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(160));
        }
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
                "{} :: {} ROWS :: {} OUTDATED :: {:.0} FPS :: INVENTORY {:.0} MS{}",
                fuide::fmt::uptime(t),
                self.rows.len(),
                outdated,
                fps,
                self.fetch_ms,
                match self.runner.job() {
                    Some(j) => format!(
                        " :: RUN {}",
                        fuide::fmt::uptime(j.started.elapsed().as_secs_f64())
                    ),
                    None => String::new(),
                }
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
        if let Some(job) = self.runner.job() {
            shell = shell.lamp(
                format!("RUNNING {}", job.label.split(" //").next().unwrap_or("")),
                pal.warn,
                true,
            );
        }
        if matches!(
            self.dialog,
            Some(OpenDialog {
                state: DialogState::Prompt { .. },
                closing: false
            })
        ) {
            shell = shell.lamp("INPUT WANTED", pal.warn, true);
        }
        if self.system.tray_running {
            shell = shell.lamp("TRAY", pal.accent, false);
        }
        if let Some((text, busy)) = self.agent.lamp() {
            shell = shell.lamp(text, if busy { pal.warn } else { pal.accent }, busy);
        }

        let console_open = self.settings.log_open;
        let mut console_resized = false;
        let mut console_toggled = false;
        let out = shell.show_full(ui, |ui| {
            let c = ui.max_rect();
            let top = c.top() + 10.0;
            let left = Rect::from_min_max(pos2(c.left(), top), pos2(c.left() + LEFT_W, c.bottom()));
            let right =
                Rect::from_min_max(pos2(c.right() - RIGHT_W, top), pos2(c.right(), c.bottom()));
            let center = Rect::from_min_max(
                pos2(left.right() + GAP, top),
                pos2(right.left() - GAP, c.bottom()),
            );
            let views = Rect::from_min_size(left.min, vec2(left.width(), VIEWS_H));
            let system =
                Rect::from_min_max(pos2(left.left(), views.bottom() + GAP + 8.0), left.max);
            let events =
                Rect::from_min_max(pos2(right.left(), right.bottom() - EVENTS_H), right.max);
            let inspector =
                Rect::from_min_max(right.min, pos2(right.right(), events.top() - GAP - 8.0));
            let console_max = center.height() - TOOLBAR_H - 12.0 - TABLE_MIN;
            self.console_h = self
                .console_h
                .clamp(CONSOLE_MIN, console_max.max(CONSOLE_MIN));
            let console_h = if console_open {
                self.console_h
            } else {
                CONSOLE_CLOSED
            };
            let console =
                Rect::from_min_max(pos2(center.left(), center.bottom() - console_h), center.max);
            let toolbar = Rect::from_min_size(
                pos2(center.left(), center.top() - 8.0),
                vec2(center.width(), TOOLBAR_H),
            );
            let listing = Rect::from_min_max(
                pos2(center.left(), toolbar.bottom() + 12.0),
                pos2(center.right(), console.top() - GAP - 8.0),
            );

            self.ui_views(ui, views, &mut actions);
            self.ui_system(ui, system, &mut actions);
            self.ui_toolbar(ui, toolbar, &mut actions);
            self.ui_listing(ui, listing, &mut actions);
            self.ui_inspector(ui, inspector, &mut actions);
            self.ui_events(ui, events);
            if console_open {
                let strip = Rect::from_min_max(
                    pos2(center.left(), listing.bottom()),
                    pos2(center.right(), console.top()),
                );
                let resp = widgets::h_splitter(
                    ui,
                    strip,
                    "console",
                    &mut self.console_h,
                    CONSOLE_MIN,
                    console_max,
                    "CONSOLE HEIGHT",
                );
                console_resized = resp.drag_stopped();
            }
            console_toggled = self.ui_console(ui, console, console_open, &mut actions);
        });
        self.agent.paint(&ctx);
        if out.settings_clicked {
            actions.push(Action::OpenSettings);
        }
        if console_resized {
            self.settings.log_height = Some(self.console_h.round());
            self.save_settings(t);
        }
        if console_toggled {
            self.settings.log_open = !console_open;
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
    /// `FUIDE_DEV_DIALOG=install|remove|upgrade|password|error|success|abort`.
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
            "password" => {
                self.dialog = Some(OpenDialog {
                    state: DialogState::Prompt {
                        prompt: Prompt::Password {
                            text: "[sudo] password for kobago:".into(),
                        },
                        key: PromptKey {
                            row: 0,
                            text: "dev".into(),
                        },
                        input: String::new(),
                        selected: Vec::new(),
                    },
                    closing: false,
                })
            }
            "abort" => {
                self.dialog = Some(OpenDialog {
                    state: DialogState::Abort,
                    closing: false,
                })
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
        let busy = self.runner.running();
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
                    &self
                        .system
                        .privilege
                        .as_deref()
                        .map(|p| {
                            std::path::Path::new(p)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| p.to_string())
                        })
                        .unwrap_or_else(|| "NONE".into()),
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
        let busy = self.runner.running();
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
                                    Status::Outdated => pal.warn,
                                    Status::Orphan => pal.warn,
                                    Status::Explicit => pal.ok.gamma_multiply(0.8),
                                    Status::Dependency => pal.text_dim,
                                    Status::Available => pal.text_dim,
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
        let busy = self.runner.running();
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
                    if p.installed {
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

    fn ui_events(&self, ui: &mut Ui, rect: Rect) {
        let pal = palette(ui.ctx());
        Panel::new("Event log")
            .tag(format!("{} lines", self.log.len()), pal.text_dim)
            .padding(8.0, 12.0)
            .show_rect(ui, rect, |ui| {
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
                    type_scale(ui.ctx()).small,
                    widgets::LogOrder::NewestFirst,
                );
            });
    }

    fn ui_console(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        open: bool,
        actions: &mut Vec<Action>,
    ) -> bool {
        let pal = palette(ui.ctx());
        let ts = type_scale(ui.ctx());
        let running = self.runner.running();
        let tag = match self.runner.job() {
            Some(j) => j.label.clone(),
            None => format!("{} lines", self.term.len()),
        };
        let input_h = ts.row + 8.0;
        let (_, toggled) = Panel::new("Console")
            .tag(tag, if running { pal.warn } else { pal.text_dim })
            .padding(8.0, 12.0)
            .show_collapsible_rect(ui, rect, open, |ui| {
                if !open {
                    return;
                }
                let inner = ui.max_rect();
                let ch = ui
                    .painter()
                    .layout_no_wrap("0".into(), mono(ts.label), pal.text)
                    .size()
                    .x
                    .max(1.0);
                let cols = ((inner.width() - 12.0) / ch).floor() as u16;
                let rows = ((inner.height() - input_h) / (ts.label + 4.0)).floor() as u16;
                self.runner.resize(cols, rows);
                let out_rect =
                    Rect::from_min_max(inner.min, pos2(inner.right(), inner.bottom() - input_h));
                let in_rect = Rect::from_min_max(
                    pos2(inner.left(), inner.bottom() - input_h + 4.0),
                    inner.max,
                );
                let mut out = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("console-out")
                        .max_rect(out_rect),
                );
                out.set_clip_rect(out_rect.intersect(ui.clip_rect()));
                console_lines(&mut out, &self.term, &pal, ts.label);
                let mut inp = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("console-in")
                        .max_rect(in_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                inp.spacing_mut().item_spacing.x = 6.0;
                {
                    let ui = &mut inp;
                    let btn_w = 4.0 * 52.0 + 3.0 * 6.0 + 6.0 + 84.0;
                    let w = (ui.available_width() - btn_w).max(80.0);
                    let hint = if running {
                        "console input :: enter sends a line"
                    } else {
                        "no process :: root commands run here"
                    };
                    let resp = widgets::text_input(ui, w, &mut self.console_input, hint);
                    if self.console_focus_pending && running {
                        resp.request_focus();
                        self.console_focus_pending = false;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let text = std::mem::take(&mut self.console_input);
                        actions.push(Action::SendLine(text));
                        resp.request_focus();
                    }
                    for (label, send) in [("Y", "y"), ("N", "n"), ("ENTER", "")] {
                        if widgets::button(ui, vec2(52.0, ts.row), label, running).clicked() {
                            actions.push(Action::SendLine(send.into()));
                        }
                    }
                    let mut english = self.options.english;
                    if widgets::toggle_chip(ui, "C locale", &mut english)
                        .on_hover_text(
                            "Run commands under LC_ALL=C.UTF-8 so their prompts are recognised",
                        )
                        .clicked()
                    {
                        actions.push(Action::ToggleEnglish);
                    }
                    if widgets::button_colored(ui, vec2(52.0, ts.row), "^C", running, pal.danger)
                        .clicked()
                    {
                        actions.push(Action::Abort);
                    }
                }
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
                        ui.add(
                            egui::Label::new(
                                RichText::new(&c.line)
                                    .font(mono(ts.data + 2.0))
                                    .color(pal.accent),
                            )
                            .wrap(),
                        );
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
                        ui.add(
                            egui::Label::new(
                                RichText::new(&c.note).font(mono(ts.label)).color(color),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        fuide::dialog::button_row(
                            ui,
                            &[("CANCEL", pal.text_dim, true), (&c.verb, color, true)],
                        )
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
            DialogState::Prompt {
                prompt,
                input,
                selected,
                ..
            } => {
                let color = if prompt.dangerous() {
                    pal.danger
                } else if prompt.consequential() {
                    pal.warn
                } else {
                    pal.accent
                };
                let title = prompt.title();
                let verb = prompt.verb();
                let is_select = matches!(prompt, Prompt::Select { .. });
                let resp = Dialog::new(&title)
                    .tag("console", pal.text_dim)
                    .outline(color)
                    .width(if is_select { 560.0 } else { 480.0 })
                    .show(ctx, open, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add(
                            egui::Label::new(
                                RichText::new(prompt.text())
                                    .font(mono(ts.data + 1.0))
                                    .color(pal.accent),
                            )
                            .wrap(),
                        );
                        ui.add_space(4.0);
                        let mut clicked: Option<usize> = None;
                        let mut extra: Option<String> = None;
                        match prompt {
                            Prompt::YesNo { default_yes, .. } => {
                                widgets::rule(ui);
                                note(
                                    ui,
                                    if *default_yes {
                                        "ENTER = YES  ::  ESC = USE THE CONSOLE LINE"
                                    } else {
                                        "ENTER = YES  ::  THE COMMAND'S DEFAULT IS NO"
                                    },
                                    color,
                                );
                                ui.add_space(8.0);
                                clicked = fuide::dialog::button_row(
                                    ui,
                                    &[("NO", pal.text_dim, true), (verb, color, true)],
                                );
                            }
                            Prompt::Continue { .. } => {
                                widgets::rule(ui);
                                ui.add_space(8.0);
                                clicked = fuide::dialog::button_row(ui, &[(verb, color, true)])
                                    .map(|_| 1);
                            }
                            Prompt::Password { .. } => {
                                let resp =
                                    secret_input(ui, ui.available_width(), input, "password");
                                if !resp.has_focus() && open {
                                    resp.request_focus();
                                }
                                widgets::rule(ui);
                                note(
                                    ui,
                                    "SENT TO THE TERMINAL ONLY :: NEVER STORED OR LOGGED",
                                    pal.text_dim,
                                );
                                ui.add_space(8.0);
                                clicked = fuide::dialog::button_row(
                                    ui,
                                    &[
                                        ("CANCEL", pal.text_dim, true),
                                        (verb, color, !input.is_empty()),
                                    ],
                                );
                            }
                            Prompt::Input { .. } => {
                                let resp =
                                    widgets::text_input(ui, ui.available_width(), input, "answer");
                                if !resp.has_focus() && open {
                                    resp.request_focus();
                                }
                                widgets::rule(ui);
                                note(ui, "ENTER SENDS THE TEXT (EMPTY = THE DEFAULT)", color);
                                ui.add_space(8.0);
                                clicked = fuide::dialog::button_row(
                                    ui,
                                    &[("CANCEL", pal.text_dim, true), (verb, color, true)],
                                );
                            }
                            Prompt::Select {
                                items,
                                all_label,
                                enter_label,
                                ..
                            } => {
                                if items.is_empty() {
                                    let resp = widgets::text_input(
                                        ui,
                                        ui.available_width(),
                                        input,
                                        "numbers, e.g. 1 3 5 (0 = all)",
                                    );
                                    if !resp.has_focus() && open {
                                        resp.request_focus();
                                    }
                                } else {
                                    let h = (items.len().min(8) as f32) * (ts.row + 2.0) + 4.0;
                                    egui::ScrollArea::vertical()
                                        .id_salt("select")
                                        .max_height(h)
                                        .min_scrolled_height(h)
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            for (i, item) in items.iter().enumerate() {
                                                check_row(ui, item, &mut selected[i], pal.accent);
                                            }
                                        });
                                }
                                widgets::rule(ui);
                                let n = selected.iter().filter(|s| **s).count();
                                note(
                                    ui,
                                    &format!(
                                        "{n} SELECTED  ::  {enter_label} = ENTER WITHOUT A CHOICE"
                                    ),
                                    color,
                                );
                                ui.add_space(8.0);
                                let sel_enabled = if items.is_empty() {
                                    !input.is_empty()
                                } else {
                                    n > 0
                                };
                                let c = fuide::dialog::button_row(
                                    ui,
                                    &[
                                        (enter_label.as_str(), pal.text_dim, true),
                                        (all_label.as_str(), pal.accent, true),
                                        (verb, color, sel_enabled),
                                    ],
                                );
                                match c {
                                    Some(0) => extra = Some(String::new()),
                                    Some(1) => extra = Some("0".into()),
                                    Some(2) => {
                                        if items.is_empty() {
                                            extra = Some(input.clone())
                                        } else {
                                            clicked = Some(1)
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        (clicked, extra)
                    });
                finished = resp.finished;
                let (clicked, extra) = resp.inner.unwrap_or((None, None));
                if !open {
                } else if let Some(text) = extra {
                    actions.push(Action::Answer(text));
                } else if resp.should_close {
                    actions.push(Action::CloseDialog);
                } else if clicked == Some(0) {
                    if matches!(prompt, Prompt::YesNo { .. }) {
                        actions.push(Action::Answer("n".into()));
                    } else {
                        actions.push(Action::Dismiss);
                    }
                } else if enter || clicked == Some(1) {
                    actions.push(Action::ConfirmDialog);
                }
            }
            DialogState::Abort => {
                let resp = Dialog::new("Abort")
                    .tag("console", pal.text_dim)
                    .outline(pal.danger)
                    .width(460.0)
                    .show(ctx, open, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add(
                            egui::Label::new(
                                RichText::new("Send Ctrl+C to the running command")
                                    .font(mono(ts.data + 1.0))
                                    .color(pal.accent),
                            )
                            .wrap(),
                        );
                        widgets::rule(ui);
                        note(
                            ui,
                            "STOPPING MID-TRANSACTION CAN LEAVE PACKAGES HALF-INSTALLED",
                            pal.danger,
                        );
                        ui.add_space(8.0);
                        fuide::dialog::button_row(
                            ui,
                            &[
                                ("KEEP RUNNING", pal.text_dim, true),
                                ("ABORT", pal.danger, true),
                            ],
                        )
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
                let resp = fuide::dialog::alert(
                    ctx,
                    open,
                    word,
                    line,
                    "details :: console and event log",
                    color,
                );
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
        mono(ts.label),
        color,
    );
}

fn check_row(ui: &mut Ui, label: &str, on: &mut bool, accent: egui::Color32) -> egui::Response {
    let pal = palette(ui.ctx());
    let ts = type_scale(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ts.row), Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    fuide::agent::describe(&resp, || {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *on, label.to_string())
    });
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, egui::CornerRadius::ZERO, accent.gamma_multiply(0.08));
    }
    let bx = Rect::from_center_size(pos2(r.left() + 10.0, r.center().y), vec2(12.0, 12.0));
    p.rect_stroke(
        bx,
        egui::CornerRadius::ZERO,
        Stroke::new(1.0, accent.gamma_multiply(if *on { 1.0 } else { 0.5 })),
        egui::StrokeKind::Inside,
    );
    if *on {
        p.rect_filled(bx.shrink(3.0), egui::CornerRadius::ZERO, accent);
    }
    p.with_clip_rect(r).text(
        pos2(r.left() + 24.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        mono(ts.data),
        if *on { pal.accent } else { pal.text },
    );
    resp
}

/// Masked password field, never described with its value.
fn secret_input(ui: &mut Ui, width: f32, text: &mut String, hint: &str) -> egui::Response {
    let pal = palette(ui.ctx());
    let ts = type_scale(ui.ctx());
    let (frame_rect, _) = ui.allocate_exact_size(vec2(width, ts.row), Sense::hover());
    ui.painter().rect_filled(
        frame_rect,
        egui::CornerRadius::ZERO,
        pal.bg_deep.gamma_multiply(0.6),
    );
    let outline_idx = ui.painter().add(egui::Shape::Noop);
    let inner = frame_rect.shrink2(vec2(6.0, 2.0));
    let resp = ui.put(
        inner,
        egui::TextEdit::singleline(text)
            .password(true)
            .frame(egui::Frame::NONE)
            .font(mono(ts.data))
            .text_color(pal.accent)
            .hint_text(
                RichText::new(hint.to_uppercase())
                    .font(mono(ts.label))
                    .color(pal.text_dim),
            )
            .desired_width(f32::INFINITY),
    );
    let mut info = egui::WidgetInfo::text_edit(true, "", "", hint);
    info.label = Some(hint.to_uppercase());
    fuide::agent::describe(&resp, || info);
    let a = if resp.has_focus() { 0.9 } else { 0.35 };
    ui.painter().set(
        outline_idx,
        egui::Shape::rect_stroke(
            frame_rect,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0, pal.accent.gamma_multiply(a)),
            egui::StrokeKind::Inside,
        ),
    );
    resp
}

fn hue_color(style: crate::term::Style, pal: &theme::Palette) -> egui::Color32 {
    let c = match style.hue {
        Hue::Default | Hue::White => pal.text,
        Hue::Red => pal.danger,
        Hue::Green => pal.ok,
        Hue::Yellow => pal.warn,
        Hue::Blue | Hue::Cyan | Hue::Magenta => pal.accent,
        Hue::Black => pal.text_dim,
    };
    if style.dim {
        pal.text_dim
    } else if style.bold || style.hue != Hue::Default {
        c
    } else {
        c.gamma_multiply(0.85)
    }
}

fn console_lines(ui: &mut Ui, term: &Terminal, pal: &theme::Palette, size: f32) {
    let total = term.len();
    let start = total.saturating_sub(CONSOLE_VISIBLE);
    egui::ScrollArea::vertical()
        .id_salt("console-scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
            ui.style_mut().interaction.selectable_labels = true;
            ui.style_mut().interaction.multi_widget_text_select = true;
            if start > 0 {
                ui.add(egui::Label::new(
                    RichText::new(format!("{start} OLDER LINES NOT SHOWN"))
                        .font(mono(size))
                        .color(pal.text_dim),
                ));
            }
            if term.is_empty() {
                ui.add(egui::Label::new(
                    RichText::new(
                        "PACMAN AND THE AUR HELPER RUN HERE :: THEIR PROMPTS BECOME DIALOGS",
                    )
                    .font(mono(size))
                    .color(pal.text_dim),
                ));
            }
            for i in start..total {
                let runs = term.runs(i);
                if runs.is_empty() {
                    ui.add(egui::Label::new(RichText::new(" ").font(mono(size))));
                    continue;
                }
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = ui.available_width();
                for r in runs {
                    job.append(
                        &r.text,
                        0.0,
                        egui::TextFormat {
                            font_id: mono(size),
                            color: hue_color(r.style, pal),
                            ..Default::default()
                        },
                    );
                }
                ui.add(egui::Label::new(job).wrap());
            }
        });
}

#[cfg(test)]
mod e2e;
#[cfg(test)]
mod tests;
