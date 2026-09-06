//! State-machine tests for the package console, against the fake toolchain in `fixtures/`
//! (`fake-pacman.sh`, `fake-yay.sh`, `fake-sudo.sh`, `fake-checkupdates.sh`) so the real
//! worker threads, the pty runner, prompt detection and the dialogs are exercised end to end
//! without touching the system.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use super::*;

pub(super) const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

/// Route pacman / yay / sudo / checkupdates to the fake scripts and the state to a temp dir
/// (process-wide; every test uses the same values).
pub(super) fn use_fake_toolchain() -> PathBuf {
    static ONCE: Once = Once::new();
    let state = std::env::temp_dir().join(format!("fuide-arch-update-test-{}", std::process::id()));
    ONCE.call_once(|| {
        let f = Path::new(FIXTURES);
        std::fs::create_dir_all(&state).unwrap();
        // SAFETY: set once before any worker thread is spawned
        unsafe {
            std::env::set_var("FUIDE_ARCH_PACMAN", f.join("fake-pacman.sh"));
            std::env::set_var("FUIDE_ARCH_AUR_HELPER", f.join("fake-yay.sh"));
            std::env::set_var("FUIDE_ARCH_SUDO", f.join("fake-sudo.sh"));
            std::env::set_var("FUIDE_ARCH_CHECKUPDATES", f.join("fake-checkupdates.sh"));
            std::env::set_var("FUIDE_ARCH_PACCACHE", "none");
            std::env::set_var("FUIDE_ARCH_STATE_DIR", &state);
            std::env::set_var("FUIDE_ARCH_SYNC_DIR", &state);
            std::env::remove_var("FAKE_SLOW");
        }
    });
    state
}

/// Tests that run the fakes share their knobs (`FAKE_*`) and state files: take turns.
pub(super) fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wipe the fake state (removed / added / upgraded marks) between tests.
pub(super) fn reset_fake_state(state: &Path) {
    for f in [
        "fake-removed",
        "fake-added",
        "fake-marked",
        "fake-upgraded",
        "fake-password-seen",
        "fake-yay-log",
        "check",
    ] {
        let _ = std::fs::remove_file(state.join(f));
    }
}

pub(super) fn app() -> (egui::Context, PkgApp) {
    let ctx = egui::Context::default();
    let app = PkgApp::with_context(&ctx, Settings::default(), Options::default());
    (ctx, app)
}

/// Pump until no worker and no console command is in flight.
pub(super) fn pump(ctx: &egui::Context, app: &mut PkgApp) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut t = 0.0;
    loop {
        t += 0.05;
        app.poll(ctx, t);
        app.check_prompt(t + 1.0);
        if !app.runner.running() && !app.backend.busy() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "workers did not finish; console:\n{}",
            console(app)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    if app.dirty {
        app.rebuild_rows();
    }
}

fn wait_prompt(ctx: &egui::Context, app: &mut PkgApp) -> Prompt {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut t = 0.0;
    loop {
        t += 0.05;
        app.poll(ctx, t);
        app.check_prompt(t + 1.0);
        if let Some(OpenDialog {
            state: DialogState::Prompt { prompt, .. },
            closing: false,
        }) = &app.dialog
        {
            return prompt.clone();
        }
        assert!(
            app.runner.running(),
            "command exited before asking; console:\n{}",
            console(app)
        );
        assert!(
            Instant::now() < deadline,
            "no prompt; console:\n{}",
            console(app)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn console(app: &PkgApp) -> String {
    (0..app.term.len())
        .map(|i| app.term.text(i))
        .collect::<Vec<_>>()
        .join("\n")
}

fn log_has(app: &PkgApp, needle: &str) -> bool {
    app.log.iter().any(|e| e.text.contains(needle))
}

fn confirm_job(app: &PkgApp) -> Job {
    match &app.dialog {
        Some(OpenDialog {
            state: DialogState::Confirm(c),
            ..
        }) => c.job.clone(),
        other => panic!(
            "no confirm dialog: {}",
            other.as_ref().map(|d| d.closing).is_some()
        ),
    }
}

/// Start with the inventory + check loaded.
fn loaded() -> (egui::Context, PkgApp, PathBuf) {
    let state = use_fake_toolchain();
    reset_fake_state(&state);
    let (ctx, mut app) = app();
    app.backend.fetch_inventory(ctx.clone());
    app.backend.fetch_system(ctx.clone());
    app.backend.check(ctx.clone());
    pump(&ctx, &mut app);
    (ctx, app, state)
}

fn names(app: &PkgApp) -> Vec<&str> {
    app.rows
        .iter()
        .map(|&i| app.source()[i].name.as_str())
        .collect()
}

// ---------------------------------------------------------------- pure

#[test]
fn status_precedence() {
    let mut p = Package::default();
    assert_eq!(status_of(&p), Status::Available);
    p.installed = true;
    p.reason = Reason::Explicit;
    assert_eq!(status_of(&p), Status::Explicit);
    p.reason = Reason::Dependency;
    assert_eq!(status_of(&p), Status::Dependency);
    p.orphan = true;
    assert_eq!(status_of(&p), Status::Orphan);
    p.latest = Some("2".into());
    assert_eq!(status_of(&p), Status::Outdated);
}

#[test]
fn options_round_trip() {
    let dir = std::env::temp_dir().join(format!("fuide-arch-update-opts-{}", std::process::id()));
    let path = dir.join("arch-update.app.conf");
    let o = Options { english: false };
    o.save_to(&path).unwrap();
    assert_eq!(Options::load_from(&path), o);
    assert!(Options::load_from(Path::new("/nonexistent")).english);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn injected_inventory_and_check_fill_the_views() {
    let (ctx, mut app) = app();
    let pkgs = archpkg::pacman::parse_info(
        &std::fs::read_to_string(Path::new(FIXTURES).join("qi.txt")).unwrap(),
        true,
    );
    app.backend.inject(Msg::Inventory(Ok(pkgs), 3.0));
    app.backend.inject(Msg::Check(Ok(vec![Upgrade {
        name: "ripgrep".into(),
        current: "15.2.0-1".into(),
        new: "15.3.0-1".into(),
        aur: false,
    }])));
    app.poll(&ctx, 1.0);
    app.rebuild_rows();
    assert_eq!(app.packages.len(), 5);
    assert_eq!(
        names(&app),
        vec![
            "bash",
            "orphan-lib",
            "ripgrep",
            "visual-studio-code-bin",
            "yay"
        ]
    );
    assert_eq!(app.count(View::Explicit), 3);
    assert_eq!(app.count(View::Updates), 1);
    assert!(app
        .packages
        .iter()
        .find(|p| p.name == "ripgrep")
        .unwrap()
        .outdated());
    app.apply(&ctx, Action::SetView(View::Updates), 1.0);
    app.rebuild_rows();
    assert_eq!(names(&app), vec!["ripgrep"]);
    app.apply(&ctx, Action::SetView(View::Installed), 1.0);
    app.filter = "code".into();
    app.rebuild_rows();
    assert_eq!(names(&app), vec!["visual-studio-code-bin"]);
}

// ---------------------------------------------------------------- with the fake toolchain

#[test]
fn inventory_marks_foreign_and_orphans_and_the_check_writes_the_state_file() {
    let _guard = serial();
    let (_ctx, app, state) = loaded();
    assert_eq!(app.packages.len(), 5);
    let yay = app.packages.iter().find(|p| p.name == "yay").unwrap();
    assert_eq!(yay.repo, "aur");
    assert!(
        app.packages
            .iter()
            .find(|p| p.name == "orphan-lib")
            .unwrap()
            .orphan
    );
    assert_eq!(app.count(View::Foreign), 2);
    assert_eq!(app.count(View::Orphans), 1);
    // 2 repo (fake-checkupdates) + 1 aur (fake-yay)
    assert_eq!(app.updates.len(), 3);
    assert!(app
        .packages
        .iter()
        .find(|p| p.name == "visual-studio-code-bin")
        .unwrap()
        .outdated());
    let st = archpkg::state::load_from(&state.join("check"));
    assert_eq!(st.updates.len(), 3);
    assert!(st.checked_at.is_some());
    assert!(app
        .system
        .aur_helper
        .as_deref()
        .is_some_and(|h| h.contains("fake-yay")));
    assert!(log_has(&app, "3 updates"));
}

#[test]
fn install_goes_through_sudo_password_and_pacman_confirmation() {
    let _guard = serial();
    let (ctx, mut app, state) = loaded();
    app.apply(&ctx, Action::Install("ripgrep-all".into(), false), 1.0);
    let job = confirm_job(&app);
    assert!(job.program.ends_with("fake-sudo.sh"));
    assert_eq!(job.args[1..], ["-S", "--needed", "ripgrep-all"]);
    app.apply(&ctx, Action::ConfirmDialog, 1.0);
    app.dialog = None;
    // 1. sudo password
    let p = wait_prompt(&ctx, &mut app);
    assert!(matches!(p, Prompt::Password { .. }), "{p:?}");
    app.apply(&ctx, Action::Answer("hunter2".into()), 2.0);
    app.dialog = None;
    // 2. pacman's question
    let p = wait_prompt(&ctx, &mut app);
    assert_eq!(p.title(), "PACMAN");
    assert!(matches!(
        p,
        Prompt::YesNo {
            default_yes: true,
            ..
        }
    ));
    app.apply(&ctx, Action::ConfirmDialog, 3.0);
    app.dialog = None;
    pump(&ctx, &mut app);
    assert_eq!(
        std::fs::read_to_string(state.join("fake-password-seen"))
            .unwrap()
            .trim(),
        "hunter2"
    );
    assert!(!log_has(&app, "hunter2"));
    let notice = app.notice_queue.pop_front().expect("a completion card");
    assert_eq!(notice, (true, "INSTALL // RIPGREP-ALL".into()));
    // the inventory was re-read: the new package is there
    assert!(app.packages.iter().any(|p| p.name == "ripgrep-all"));
}

#[test]
fn remove_is_a_danger_confirmation_and_declining_pacman_fails_cleanly() {
    let _guard = serial();
    let (ctx, mut app, _state) = loaded();
    app.apply(&ctx, Action::Remove("orphan-lib".into()), 1.0);
    match &app.dialog {
        Some(OpenDialog {
            state: DialogState::Confirm(c),
            ..
        }) => {
            assert!(c.danger);
            assert_eq!(c.job.args[1..], ["-Rns", "orphan-lib"]);
        }
        _ => panic!("no confirm"),
    }
    app.apply(&ctx, Action::ConfirmDialog, 1.0);
    app.dialog = None;
    let _ = wait_prompt(&ctx, &mut app); // password
    app.apply(&ctx, Action::Answer("pw".into()), 2.0);
    app.dialog = None;
    let p = wait_prompt(&ctx, &mut app); // remove?
    assert!(matches!(p, Prompt::YesNo { .. }));
    app.apply(&ctx, Action::Answer("n".into()), 3.0);
    app.dialog = None;
    pump(&ctx, &mut app);
    let notice = app.notice_queue.pop_front().expect("an error card");
    assert!(!notice.0);
    assert!(log_has(&app, "exit code 1"));
    assert!(
        app.packages.iter().any(|p| p.name == "orphan-lib"),
        "declined: still installed"
    );
}

#[test]
fn upgrade_all_uses_the_helper_and_clears_the_pending_list() {
    let _guard = serial();
    let (ctx, mut app, state) = loaded();
    assert_eq!(app.outdated_count(), 3);
    app.apply(&ctx, Action::UpgradeAll, 1.0);
    let job = confirm_job(&app);
    assert!(job.program.ends_with("fake-yay.sh"), "{job:?}");
    assert_eq!(job.args, ["-Syu"]);
    app.apply(&ctx, Action::ConfirmDialog, 1.0);
    app.dialog = None;
    // yay: diffs? → Input prompt (generic), answered with N
    let p = wait_prompt(&ctx, &mut app);
    assert!(matches!(p, Prompt::Input { .. }), "{p:?}");
    app.apply(&ctx, Action::Answer("N".into()), 2.0);
    app.dialog = None;
    let p = wait_prompt(&ctx, &mut app);
    assert!(matches!(p, Prompt::Password { .. }), "{p:?}");
    app.apply(&ctx, Action::Answer("pw".into()), 3.0);
    app.dialog = None;
    let p = wait_prompt(&ctx, &mut app);
    assert!(matches!(p, Prompt::YesNo { .. }), "{p:?}");
    app.apply(&ctx, Action::ConfirmDialog, 4.0);
    app.dialog = None;
    pump(&ctx, &mut app);
    assert!(state.join("fake-upgraded").exists());
    assert_eq!(
        app.notice_queue.pop_front().unwrap(),
        (true, "UPGRADE // ALL".into())
    );
    // the post-run check found nothing pending (fake-checkupdates sees fake-upgraded)
    assert_eq!(app.updates.iter().filter(|u| !u.aur).count(), 0);
    let st = archpkg::state::load_from(&state.join("check"));
    assert!(st.updates.iter().all(|u| u.aur));
}

#[test]
fn search_merges_repo_and_aur_and_fetches_details_on_selection() {
    let _guard = serial();
    let (ctx, mut app, _state) = loaded();
    app.apply(&ctx, Action::SetView(View::Search), 1.0);
    app.search_query = "ripgrep".into();
    app.apply(&ctx, Action::Search, 1.0);
    pump(&ctx, &mut app);
    let n = names(&app);
    assert_eq!(n, vec!["ripgrep", "ripgrep-all", "ripgrep-git"]);
    // the installed hit carries the inventory record
    let rg = app
        .search_results
        .iter()
        .find(|p| p.name == "ripgrep")
        .unwrap();
    assert!(rg.installed && rg.reason == Reason::Explicit);
    let git = app
        .search_results
        .iter()
        .find(|p| p.name == "ripgrep-git")
        .unwrap();
    assert!(git.is_aur() && !git.installed && git.votes == Some(9));
    // selecting a not-installed hit asks for -Si details
    let row = app
        .rows
        .iter()
        .position(|&i| app.search_results[i].name == "ripgrep-all")
        .unwrap();
    app.apply(&ctx, Action::Select(Some(row)), 2.0);
    pump(&ctx, &mut app);
    let all = app
        .search_results
        .iter()
        .find(|p| p.name == "ripgrep-all")
        .unwrap();
    assert_eq!(all.url, "https://example.org/ripgrep-all");
    assert_eq!(all.depends, vec!["glibc", "pcre2"]);
    assert!(!all.installed);
    let row = app
        .rows
        .iter()
        .position(|&i| app.search_results[i].name == "ripgrep-git")
        .unwrap();
    app.apply(&ctx, Action::Select(Some(row)), 3.0);
    pump(&ctx, &mut app);
    let git = app
        .search_results
        .iter()
        .find(|p| p.name == "ripgrep-git")
        .unwrap();
    assert_eq!(git.maintainer, "someone");
    assert_eq!(git.depends.len(), 2, "continuation lines joined");
}

#[test]
fn mark_explicit_runs_without_a_confirmation() {
    let _guard = serial();
    let (ctx, mut app, state) = loaded();
    app.apply(&ctx, Action::MarkExplicit("bash".into(), true), 1.0);
    assert!(app.runner.running());
    let _ = wait_prompt(&ctx, &mut app);
    app.apply(&ctx, Action::Answer("pw".into()), 2.0);
    app.dialog = None;
    pump(&ctx, &mut app);
    assert!(std::fs::read_to_string(state.join("fake-marked"))
        .unwrap()
        .contains("--asexplicit bash"));
}

#[test]
fn orphans_and_cache_confirmations() {
    let _guard = serial();
    let (ctx, mut app, _state) = loaded();
    app.apply(&ctx, Action::RemoveOrphans, 1.0);
    let job = confirm_job(&app);
    assert_eq!(job.args[1..], ["-Rns", "orphan-lib"]);
    app.apply(&ctx, Action::CloseDialog, 1.0);
    app.dialog = None;
    app.system.cache_kib = Some(1024.0);
    app.apply(&ctx, Action::CleanCache, 2.0);
    let job = confirm_job(&app);
    assert_eq!(job.args[1..], ["-rk2"]);
    assert!(job.args[0].contains("none") || job.args[0].contains("paccache"));
}

#[test]
fn start_up_select_and_upgrade_requests() {
    let _guard = serial();
    let state = use_fake_toolchain();
    reset_fake_state(&state);
    let (ctx, mut app) = app();
    app.start = StartUp {
        upgrade: true,
        select: Some("visual-studio-code-bin".into()),
    };
    app.backend.fetch_inventory(ctx.clone());
    app.backend.check(ctx.clone());
    pump(&ctx, &mut app);
    assert_eq!(app.view, View::Updates);
    assert_eq!(
        app.selected_package().map(|p| p.name.as_str()),
        Some("visual-studio-code-bin")
    );
    assert!(matches!(
        app.dialog,
        Some(OpenDialog {
            state: DialogState::Confirm(_),
            ..
        })
    ));
}
