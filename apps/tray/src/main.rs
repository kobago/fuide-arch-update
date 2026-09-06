//! fuide-arch-update-tray — systray applet for FUIDE Arch-Update.
//! https://github.com/kobago/fuide-arch-update
//! SPDX-License-Identifier: GPL-3.0-or-later
//!
//! Checks for pacman / AUR updates on a timer (`checkupdates` + `<helper> -Qua`, no root),
//! keeps the shared state file up to date, shows a StatusNotifierItem whose icon and menu
//! reflect the pending updates, and sends a desktop notification when new updates appear.
//! Left click / the notification's action open the GUI.
//!
//!   fuide-arch-update-tray [--interval SECS] [--no-initial-check]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archpkg::state::{self, CheckState};
use ksni::menu::{MenuItem, StandardItem, SubMenu};
use ksni::{Handle, TrayMethods};
use log::{error, info, warn};
use notify::{EventKind, RecursiveMode, Watcher};

const APP_NAME: &str = "FUIDE Arch-Update";
const DEFAULT_INTERVAL: u64 = 3600;
/// Delay before the first check so a fresh desktop session has its network up.
const INITIAL_DELAY: u64 = 20;
/// Menu entries per package list before it is cut with "…".
const MENU_MAX: usize = 25;

struct Tray {
    state: CheckState,
    checking: bool,
    next_check: Option<Instant>,
    gui: PathBuf,
    request_check: tokio::sync::mpsc::UnboundedSender<()>,
}

impl Tray {
    fn count(&self) -> usize {
        self.state.updates.len()
    }

    fn summary(&self) -> String {
        match (self.count(), &self.state.error) {
            (0, None) => "System is up to date".into(),
            (0, Some(e)) => format!("Check failed: {e}"),
            (1, _) => "1 update available".into(),
            (n, _) => format!("{n} updates available"),
        }
    }

    fn open_gui(&self, args: &[&str]) {
        launch_gui(&self.gui, args);
    }
}

fn launch_gui(gui: &Path, args: &[&str]) {
    match Command::new(gui).args(args).spawn() {
        Ok(_) => info!("launched {}", gui.display()),
        Err(e) => error!("cannot launch {}: {e}", gui.display()),
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "fuide-arch-update".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn title(&self) -> String {
        APP_NAME.into()
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let kind = if self.checking {
            IconKind::Checking
        } else if self.state.error.is_some() && self.count() == 0 {
            IconKind::Error
        } else if self.count() > 0 {
            IconKind::Updates
        } else {
            IconKind::Current
        };
        [16, 22, 24, 32, 48, 64]
            .into_iter()
            .map(|s| icon(s, kind))
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let mut description = self.summary();
        if let Some(t) = self.state.checked_at {
            description.push_str(&format!("\nLast check {}", archpkg::fmt_ago(t)));
        }
        ksni::ToolTip {
            title: APP_NAME.into(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.open_gui(&[]);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu: Vec<MenuItem<Self>> = Vec::new();
        let n = self.count();
        menu.push(
            StandardItem {
                label: self.summary(),
                enabled: n > 0,
                activate: Box::new(|t: &mut Self| t.open_gui(&[])),
                ..Default::default()
            }
            .into(),
        );
        let repo: Vec<&archpkg::Upgrade> = self.state.updates.iter().filter(|u| !u.aur).collect();
        let aur: Vec<&archpkg::Upgrade> = self.state.updates.iter().filter(|u| u.aur).collect();
        for (label, list) in [("Repositories", repo), ("AUR", aur)] {
            if list.is_empty() {
                continue;
            }
            let mut items: Vec<MenuItem<Self>> = list
                .iter()
                .take(MENU_MAX)
                .map(|u| {
                    let name = u.name.clone();
                    StandardItem {
                        label: format!("{}  {} → {}", u.name, u.current, u.new),
                        activate: Box::new(move |t: &mut Self| t.open_gui(&["--select", &name])),
                        ..Default::default()
                    }
                    .into()
                })
                .collect();
            if list.len() > MENU_MAX {
                items.push(
                    StandardItem {
                        label: format!("… {} more", list.len() - MENU_MAX),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
            menu.push(
                SubMenu {
                    label: format!("{label} ({})", list.len()),
                    submenu: items,
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(MenuItem::Separator);
        if let Some(t) = self.state.checked_at {
            menu.push(
                StandardItem {
                    label: format!("Last check {}", archpkg::fmt_ago(t)),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.checking {
            menu.push(
                StandardItem {
                    label: "Checking…".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else if let Some(next) = self.next_check {
            let left = next.saturating_duration_since(Instant::now());
            menu.push(
                StandardItem {
                    label: format!("Next check in {}", archpkg::fmt_duration(left)),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: format!("Open {APP_NAME}"),
                activate: Box::new(|t: &mut Self| t.open_gui(&[])),
                ..Default::default()
            }
            .into(),
        );
        menu.push(
            StandardItem {
                label: "Upgrade now…".into(),
                enabled: n > 0,
                activate: Box::new(|t: &mut Self| t.open_gui(&["--upgrade"])),
                ..Default::default()
            }
            .into(),
        );
        menu.push(
            StandardItem {
                label: "Check for updates".into(),
                enabled: !self.checking,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.request_check.send(());
                }),
                ..Default::default()
            }
            .into(),
        );
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_| {
                    info!("quit on user request");
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        );
        menu
    }
}

// ------------------------------------------------------------------ icon

#[derive(Clone, Copy, PartialEq, Eq)]
enum IconKind {
    Current,
    Updates,
    Checking,
    Error,
}

/// A square outline with an up-chevron in the FUI cyan; amber with a dot when updates wait,
/// red when the check failed, dim while checking. ARGB32 in network byte order (ksni).
fn icon(size: i32, kind: IconKind) -> ksni::Icon {
    let (r, g, b) = match kind {
        IconKind::Current => (0x00, 0xE5, 0xFF),
        IconKind::Updates => (0xFF, 0xAA, 0x28),
        IconKind::Checking => (0x5A, 0x8C, 0x9B),
        IconKind::Error => (0xFF, 0x46, 0x5A),
    };
    let n = size as usize;
    let mut data = vec![0u8; n * n * 4];
    let s = size as f32;
    let line = (s / 12.0).max(1.2);
    let mut put = |x: usize, y: usize, a: f32| {
        if x >= n || y >= n {
            return;
        }
        let i = (y * n + x) * 4;
        let a = (a.clamp(0.0, 1.0) * 255.0) as u8;
        if a > data[i] {
            data[i] = a;
            data[i + 1] = r;
            data[i + 2] = g;
            data[i + 3] = b;
        }
    };
    // distance from a point to a segment
    let seg = |px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32| -> f32 {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
        let (cx, cy) = (ax + t * dx, ay + t * dy);
        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
    };
    let m = s * 0.14; // margin
    let segments: Vec<[f32; 4]> = vec![
        // square outline
        [m, m, s - m, m],
        [s - m, m, s - m, s - m],
        [s - m, s - m, m, s - m],
        [m, s - m, m, m],
        // chevron pointing up
        [s * 0.30, s * 0.62, s * 0.50, s * 0.36],
        [s * 0.50, s * 0.36, s * 0.70, s * 0.62],
    ];
    for y in 0..n {
        for x in 0..n {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = segments
                .iter()
                .map(|q| seg(px, py, q[0], q[1], q[2], q[3]))
                .fold(f32::MAX, f32::min);
            let a = (line / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if a > 0.0 {
                put(x, y, a);
            }
            if kind == IconKind::Updates {
                // filled dot in the lower right corner
                let (cx, cy, rad) = (s * 0.80, s * 0.80, s * 0.11);
                let dd = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let a = (rad + 0.5 - dd).clamp(0.0, 1.0);
                if a > 0.0 {
                    put(x, y, a);
                }
            }
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}

// ------------------------------------------------------------------ checks + notifications

/// Run a check on a blocking thread and update the tray. Notifies when updates appeared that
/// the previous check did not have.
async fn run_check(handle: &Handle<Tray>) {
    handle.update(|t| t.checking = true).await;
    let previous: std::collections::HashSet<String> = handle
        .update(|t| t.state.updates.iter().map(|u| u.name.clone()).collect())
        .await
        .unwrap_or_default();
    let result = tokio::task::spawn_blocking(archpkg::pacman::check_all)
        .await
        .unwrap_or_else(|e| Err(format!("check task failed: {e}")));
    let mut st = state::load();
    st.checked_at = Some(archpkg::now_epoch());
    match &result {
        Ok(u) => {
            st.updates = u.clone();
            st.error = None;
        }
        Err(e) => st.error = Some(e.clone()),
    }
    if let Err(e) = state::save(&st) {
        warn!("cannot save the state file: {e}");
    }
    let count = st.updates.len();
    let fresh: Vec<String> = st
        .updates
        .iter()
        .filter(|u| !previous.contains(&u.name))
        .map(|u| u.name.clone())
        .collect();
    let gui = handle
        .update(|t| {
            t.state = st;
            t.checking = false;
            t.gui.clone()
        })
        .await;
    match &result {
        Ok(_) => info!("check: {count} pending, {} new", fresh.len()),
        Err(e) => warn!("check failed: {e}"),
    }
    if let (Some(gui), false) = (gui, fresh.is_empty()) {
        notify(count, &fresh, gui);
    }
}

/// Desktop notification with "Open" / "Upgrade now" actions (waits on its own thread).
fn notify(count: usize, fresh: &[String], gui: PathBuf) {
    let body = {
        let mut names = fresh.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        if fresh.len() > 6 {
            names.push_str(&format!(" … (+{})", fresh.len() - 6));
        }
        names
    };
    std::thread::spawn(move || {
        let summary = if count == 1 {
            "1 update available".to_string()
        } else {
            format!("{count} updates available")
        };
        let shown = notify_rust::Notification::new()
            .appname(APP_NAME)
            .summary(&summary)
            .body(&body)
            .icon("system-software-update")
            .action("open", "Open")
            .action("upgrade", "Upgrade now")
            .timeout(notify_rust::Timeout::Never)
            .show();
        match shown {
            Ok(h) => h.wait_for_action(|action| match action {
                "open" | "default" => launch_gui(&gui, &[]),
                "upgrade" => launch_gui(&gui, &["--upgrade"]),
                _ => {}
            }),
            Err(e) => warn!("notification failed: {e}"),
        }
    });
}

/// The GUI binary: next to this executable, else `fuide-arch-update` on `PATH`.
fn gui_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("fuide-arch-update");
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from("fuide-arch-update")
}

/// One instance per session: a lock file in `$XDG_RUNTIME_DIR`.
fn single_instance() -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("fuide-arch-update-tray.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    // SAFETY: flock on an fd we own (LOCK_EX | LOCK_NB)
    let r = unsafe { flock(f.as_raw_fd(), 2 | 4) };
    (r == 0).then_some(f)
}

extern "C" {
    fn flock(fd: i32, op: i32) -> i32;
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,fuide_arch_update_tray=info"),
    )
    .init();
    let mut interval = DEFAULT_INTERVAL;
    let mut initial_check = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--interval" => {
                interval = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_INTERVAL)
                    .max(60)
            }
            "--no-initial-check" => initial_check = false,
            "--version" | "-V" => {
                println!("fuide-arch-update-tray {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => warn!("unknown argument {other}"),
        }
    }
    let _lock = match single_instance() {
        Some(f) => f,
        None => {
            error!("another fuide-arch-update-tray is already running");
            std::process::exit(3);
        }
    };

    let (check_tx, mut check_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let tray = Tray {
        state: state::load(),
        checking: false,
        next_check: None,
        gui: gui_binary(),
        request_check: check_tx.clone(),
    };
    let handle = match tray.spawn().await {
        Ok(h) => Arc::new(h),
        Err(e) => {
            error!("cannot start the tray (is a StatusNotifier host running?): {e}");
            std::process::exit(1);
        }
    };
    info!("tray started; interval {interval}s");

    // the GUI rewrites the state file after its own checks / upgrades: re-read it
    let state_path = state::state_file();
    let _ = std::fs::create_dir_all(state::state_dir());
    let (fs_tx, mut fs_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    let _ = fs_tx.send(());
                }
            }
        })
        .ok();
    if let Some(w) = watcher.as_mut() {
        if let Err(e) = w.watch(&state::state_dir(), RecursiveMode::NonRecursive) {
            warn!("cannot watch the state directory: {e}");
        }
    }
    {
        let handle = Arc::clone(&handle);
        tokio::spawn(async move {
            while fs_rx.recv().await.is_some() {
                // coalesce bursts
                tokio::time::sleep(Duration::from_millis(300)).await;
                while fs_rx.try_recv().is_ok() {}
                let st = state::load_from(&state_path);
                handle
                    .update(|t| {
                        if !t.checking {
                            t.state = st;
                        }
                    })
                    .await;
            }
        });
    }

    // periodic checks + on demand
    let period = Duration::from_secs(interval);
    let mut next = Instant::now()
        + if initial_check {
            Duration::from_secs(INITIAL_DELAY)
        } else {
            period
        };
    handle.update(|t| t.next_check = Some(next)).await;
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            r = check_rx.recv() => { if r.is_none() { break; } }
        }
        run_check(&handle).await;
        next = Instant::now() + period;
        handle.update(|t| t.next_check = Some(next)).await;
    }
}
