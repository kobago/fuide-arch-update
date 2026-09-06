//! archpkg — pacman / AUR-helper access shared by the FUIDE Arch-Update GUI and its tray applet.
//!
//! Read-only queries (`pacman -Qi`, `-Ss`, `-Si`, `checkupdates`, `<helper> -Qua` …) run here
//! with `LC_ALL=C.UTF-8` and are parsed into plain structs. Mutations are *not* here: the GUI
//! runs them in a pseudo-terminal so `sudo` and pacman's prompts work as in a terminal.
//! [`state`] is the small file the GUI and the tray share (pending updates + check time).

pub mod pacman;
pub mod state;

pub use pacman::{Package, Reason, SearchHit, Upgrade};

/// Fixed-width size from kibibytes: `  1.3 GiB`.
pub fn fmt_kib(kib: f64) -> String {
    let mut v = kib;
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{:>6.1} {}", v, units[u])
}

/// `3h 22m` / `12m` / `40s` / `2d 1h`.
pub fn fmt_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (days, h, m, sec) = (s / 86_400, (s / 3_600) % 24, (s / 60) % 60, s % 60);
    if days > 0 {
        format!("{days}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{sec}s")
    }
}

/// Seconds since the epoch, now.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "3h 22m ago" from a unix time.
pub fn fmt_ago(t: u64) -> String {
    let now = now_epoch();
    format!(
        "{} ago",
        fmt_duration(std::time::Duration::from_secs(now.saturating_sub(t)))
    )
}

/// `2026-09-06 15:14` from a unix time (local time).
pub fn fmt_epoch(t: u64) -> String {
    let dt: chrono::DateTime<chrono::Local> =
        (std::time::UNIX_EPOCH + std::time::Duration::from_secs(t)).into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations() {
        assert_eq!(fmt_kib(512.0), " 512.0 KiB");
        assert_eq!(fmt_kib(1352252.0), "   1.3 GiB");
        assert_eq!(fmt_duration(std::time::Duration::from_secs(40)), "40s");
        assert_eq!(
            fmt_duration(std::time::Duration::from_secs(3 * 3600 + 22 * 60)),
            "3h 22m"
        );
        assert_eq!(
            fmt_duration(std::time::Duration::from_secs(2 * 86_400 + 3600)),
            "2d 1h"
        );
    }
}
