//! End-to-end: the real `PkgApp` driven through the accessibility tree with `egui_kittest`,
//! against the fake toolchain in `fixtures/`.

use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Vec2};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;

use super::tests::{reset_fake_state, serial, use_fake_toolchain};
use super::*;

fn harness() -> Harness<'static, PkgApp> {
    let state = use_fake_toolchain();
    reset_fake_state(&state);
    let mut h = Harness::builder()
        .with_size(Vec2::new(1320.0, 840.0))
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| {
            let mut app = PkgApp::with_context(&cc.egui_ctx, Settings::default());
            app.backend.fetch_inventory(cc.egui_ctx.clone());
            app.backend.fetch_system(cc.egui_ctx.clone());
            app.backend.check(cc.egui_ctx.clone());
            app
        });
    pump(&mut h);
    h
}

fn pump(h: &mut Harness<'static, PkgApp>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        h.run_steps(1);
        let s = h.state();
        if !s.backend.busy() && !s.dirty {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(1);
}

fn dismiss_cards(h: &mut Harness<'static, PkgApp>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.state().dialog.is_some() {
        if matches!(h.state().dialog, Some(OpenDialog { closing: false, .. })) {
            h.key_press(Key::Enter);
        }
        h.run_steps(1);
        assert!(Instant::now() < deadline, "card did not close");
    }
    h.run_steps(1);
}

fn wait_enabled(h: &mut Harness<'static, PkgApp>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        h.run_steps(1);
        if h.query_all_by_label(label)
            .any(|n| !n.accesskit_node().is_disabled())
        {
            return;
        }
        assert!(Instant::now() < deadline, "no enabled {label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn log_has(h: &Harness<'static, PkgApp>, needle: &str) -> bool {
    h.state().log.iter().any(|e| e.text.contains(needle))
}

#[test]
fn shell_and_views_are_in_the_tree() {
    let _guard = serial();
    let mut h = harness();
    h.run_steps(2);
    for label in [
        "INSTALLED  5",
        "UPDATES  3",
        "ORPHANS  1",
        "SEARCH  0",
        "CLOSE WINDOW",
        "SETTINGS",
        "REFRESH",
        "EVENT LOG",
        "CHECK",
    ] {
        assert!(h.query_all_by_label(label).count() > 0, "{label} missing");
    }
}

#[test]
fn selecting_a_row_shows_the_inspector_and_remove_asks_first() {
    let _guard = serial();
    let mut h = harness();
    h.get_by_label("orphan-lib").click();
    h.run_steps(2);
    assert_eq!(
        h.state()
            .selected_package()
            .map(|p| p.name.clone())
            .as_deref(),
        Some("orphan-lib")
    );
    h.get_by_role_and_label(Role::Button, "REMOVE").click();
    h.run_steps(2);
    assert!(matches!(
        h.state().dialog,
        Some(OpenDialog {
            state: DialogState::Confirm(_),
            closing: false
        })
    ));
    wait_enabled(&mut h, "CANCEL");
    h.get_by_role_and_label(Role::Button, "CANCEL").click();
    h.run_steps(12);
    assert!(h.state().dialog.is_none());
    assert!(h.state().backend.running().is_none());
}

#[test]
fn upgrade_all_runs_to_the_success_card() {
    let _guard = serial();
    let mut h = harness();
    h.get_by_role_and_label(Role::Button, "UPGRADE ALL  3")
        .click();
    h.run_steps(2);
    wait_enabled(&mut h, "CANCEL");
    // Enter confirms the dialog; the command streams into the log and ends in a card
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(h.state().backend.running().is_some());
    pump(&mut h);
    assert!(log_has(&h, "UPGRADE // ALL :: done"));
    assert!(log_has(&h, ":: Starting full system upgrade..."));
    assert!(matches!(
        h.state().dialog,
        Some(OpenDialog {
            state: DialogState::Notice { success: true, .. },
            ..
        })
    ));
    dismiss_cards(&mut h);
    assert_eq!(h.state().outdated_count(), 0);
}

#[test]
fn settings_window_changes_the_palette() {
    let _guard = serial();
    let mut h = harness();
    h.get_by_label("SETTINGS").click();
    h.run_steps(3);
    h.get_by_label("GREEN  PHOSPHOR TERMINAL").click();
    h.run_steps(3);
    assert_eq!(h.state().settings.palette, PaletteKind::Green);
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().settings_win.is_open());
}
