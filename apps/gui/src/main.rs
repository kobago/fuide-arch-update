//! fuide-arch-update — package manager for Arch Linux (pacman + AUR helper) with a Sci-Fi FUI look.
//! https://github.com/kobago/fuide-arch-update
//! SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod backend;
mod instance;
mod setup;

fn main() -> eframe::Result {
    // `fuide-arch-update --mcp`: stdio MCP bridge to the running app (see `fuide::agent::bridge`)
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        std::process::exit(fuide::agent::bridge::run(app::APP_ID, app::APP_NAME));
    }
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("fuide-arch-update {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Per-user desktop integration (start menu, icon, tray at login) for a `cargo install`
        // copy: see `setup.rs`.
        Some("--setup") => std::process::exit(setup::setup()),
        Some("--unsetup") => std::process::exit(setup::unsetup()),
        _ => {}
    }
    // `--upgrade` (tray: "Upgrade now"): open with the full-upgrade confirmation up.
    // `--select NAME` (tray: a package entry): open the Updates view on that package.
    let mut start = app::StartUp::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--upgrade" => start.upgrade = true,
            "--select" => start.select = args.next(),
            _ => {}
        }
    }
    // One window per session: a second launch hands its request to the running one and exits.
    let guard = match instance::claim(
        &instance::runtime_dir(),
        &instance::Request::from_start(&start),
    ) {
        instance::Claim::Primary(g) => g,
        instance::Claim::Forwarded => return Ok(()),
    };
    fuide::devshot::install_trace_logger();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::APP_NAME)
            .with_app_id("fuide-arch-update")
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([1040.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        app::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::PkgApp::new(cc, start, guard)))),
    )
}
