//! Recognise the interactive prompts pacman, AUR helpers and `sudo` print, so the UI can offer
//! a dialog instead of a blinking cursor.
//!
//! Detection is purely textual: it looks at the line the cursor is waiting on (and, for
//! numbered selections, the lines above it). Anything unrecognised is left to the console's
//! free-form input line, so a new upstream prompt still works — it just is not pretty yet.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// `[Y/n]` / `[y/N]`. `default_yes` is what Enter alone means.
    YesNo { text: String, default_yes: bool },
    /// "Select the news to read (e.g. 1 3 5), select 0 to read them all or press enter …".
    /// `items` are the numbered lines above the prompt, in order (index 0 = item 1).
    Select {
        text: String,
        items: Vec<String>,
        all_label: String,
        enter_label: String,
    },
    /// `Press "enter" to continue` / `to quit`.
    Continue { text: String, quit: bool },
    /// `[sudo] password for user:` and friends.
    Password { text: String },
    /// Free text with an optional default (`Enter a selection (default=all):`).
    Input { text: String, default: String },
}

impl Prompt {
    pub fn text(&self) -> &str {
        match self {
            Prompt::YesNo { text, .. }
            | Prompt::Select { text, .. }
            | Prompt::Continue { text, .. }
            | Prompt::Password { text }
            | Prompt::Input { text, .. } => text,
        }
    }

    /// Short upper-case title for the dialog chip.
    pub fn title(&self) -> String {
        let t = self.text();
        let lower = t.to_lowercase();
        let pick = |s: &str| s.to_string();
        match self {
            Prompt::Password { .. } => pick("AUTHENTICATE"),
            Prompt::Continue { quit: true, .. } => pick("FINISHED"),
            Prompt::Continue { .. } => pick("CONTINUE"),
            Prompt::Select { .. } if lower.contains("news") => pick("ARCH NEWS"),
            Prompt::Select { .. } if lower.contains("service") => pick("RESTART SERVICES"),
            Prompt::Select { .. } => pick("SELECT"),
            Prompt::YesNo { .. } if lower.contains("proceed with update") => {
                pick("PROCEED WITH UPDATE")
            }
            Prompt::YesNo { .. } if lower.contains("orphan") => pick("ORPHAN PACKAGES"),
            Prompt::YesNo { .. } if lower.contains("cache") => pick("PACKAGE CACHE"),
            Prompt::YesNo { .. } if lower.contains("reboot") => pick("REBOOT"),
            Prompt::YesNo { .. } if lower.contains("pacnew") || lower.contains("process th") => {
                pick("PACNEW FILES")
            }
            Prompt::YesNo { .. } if lower.contains("flatpak") => pick("FLATPAK"),
            Prompt::YesNo { .. } if lower.contains("proceed with installation") => pick("PACMAN"),
            Prompt::YesNo { .. } if lower.contains("remove these packages") => pick("PACMAN"),
            Prompt::YesNo { .. }
                if lower.contains("proceed to review")
                    || lower.contains("diffs")
                    || lower.contains("import")
                    || lower.contains("clean build") =>
            {
                pick("AUR HELPER")
            }
            Prompt::YesNo { .. } => pick("CONFIRM"),
            Prompt::Input { .. } => pick("INPUT"),
        }
    }

    /// The verb on the dialog's primary button.
    pub fn verb(&self) -> &'static str {
        let lower = self.text().to_lowercase();
        match self {
            Prompt::Password { .. } => "AUTHENTICATE",
            Prompt::Continue { quit: true, .. } => "CLOSE",
            Prompt::Continue { .. } => "CONTINUE",
            Prompt::Select { .. } => "SELECTED",
            Prompt::YesNo { .. } if lower.contains("reboot") => "REBOOT NOW",
            Prompt::YesNo { .. } if lower.contains("remove") => "REMOVE",
            Prompt::YesNo { .. } if lower.contains("update") => "UPDATE",
            Prompt::YesNo { .. } if lower.contains("install") => "INSTALL",
            Prompt::YesNo { .. } if lower.contains("process") => "PROCESS",
            Prompt::YesNo { .. } => "YES",
            Prompt::Input { .. } => "SEND",
        }
    }

    /// Consequential (needs the warning colour): anything that changes the system.
    pub fn consequential(&self) -> bool {
        match self {
            Prompt::YesNo { .. } => true,
            Prompt::Password { .. }
            | Prompt::Select { .. }
            | Prompt::Continue { .. }
            | Prompt::Input { .. } => false,
        }
    }

    /// Irreversible / disruptive: rendered in the danger colour.
    pub fn dangerous(&self) -> bool {
        let lower = self.text().to_lowercase();
        matches!(self, Prompt::YesNo { .. })
            && (lower.contains("reboot") || lower.contains("remove"))
    }
}

/// Strip the script's `==> ` / `-> ` / `:: ` prefixes and the `[Y/n]` tail for display.
pub fn clean(line: &str) -> String {
    let mut s = line.trim();
    for p in ["==> ", "-> ", ":: "] {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest;
        }
    }
    s.trim().to_string()
}

/// Detect a prompt on `cursor` (the cursor line, trimmed). `above` are the lines preceding it,
/// oldest first (used for numbered selections).
pub fn detect(above: &[String], cursor: &str) -> Option<Prompt> {
    let line = cursor.trim();
    if line.is_empty() {
        return None;
    }
    // ASCII-only lowering keeps byte offsets identical to `line`
    let lower = line.to_ascii_lowercase();

    // passwords (sudo, sudo-rs, doas, systemd-ask-password / run0 without a polkit agent)
    if (lower.contains("password") || line.contains("パスワード"))
        && lower.trim_end_matches(' ').ends_with(':')
    {
        return Some(Prompt::Password { text: clean(line) });
    }
    if lower.starts_with("[sudo]") || lower.starts_with("[sudo:") {
        return Some(Prompt::Password { text: clean(line) });
    }

    // yes / no — arch-update, pacman (`[Y/n]`), paru (`[Y/n]:`)
    let tail = lower.trim_end_matches([' ', ':']);
    if let Some(pos) = tail.rfind("[y/n]") {
        if pos + 5 >= tail.len() {
            // `[Y/n]` = Enter means yes, `[y/N]` = Enter means no, `[y/n]` = no default (treated as no)
            let raw = &line[pos..pos + 5];
            return Some(Prompt::YesNo {
                text: clean(&line[..pos]),
                default_yes: raw.contains('Y'),
            });
        }
    }

    // numbered selection (news / services): items are `N - text` lines above
    if lower.starts_with("-> select") || lower.contains("select the") {
        let mut items: Vec<(usize, String)> = Vec::new();
        for l in above.iter().rev() {
            let t = l.trim();
            if t.is_empty() {
                if items.is_empty() {
                    continue;
                }
                // one blank line between the list and the prompt is normal; a second ends the list
                continue;
            }
            match parse_numbered(t) {
                Some((n, text)) => items.push((n, text)),
                None => {
                    if !items.is_empty() {
                        break;
                    }
                    // the prompt may be re-asked after "Invalid input"; keep looking a little
                    if items.is_empty() && (t.contains("Invalid input") || t.contains("WARNING")) {
                        continue;
                    }
                    break;
                }
            }
        }
        items.reverse();
        // must be 1..n in order
        let ok = !items.is_empty() && items.iter().enumerate().all(|(i, (n, _))| *n == i + 1);
        let items: Vec<String> = if ok {
            items.into_iter().map(|(_, t)| t).collect()
        } else {
            Vec::new()
        };
        let all_label = if lower.contains("read them all") {
            "READ ALL".into()
        } else if lower.contains("restart them all") {
            "RESTART ALL".into()
        } else {
            "ALL".into()
        };
        let enter_label = if lower.contains("proceed with update") {
            "PROCEED WITH UPDATE".into()
        } else if lower.contains("without restarting") {
            "SKIP".into()
        } else if lower.contains("quit") {
            "QUIT".into()
        } else {
            "SKIP".into()
        };
        return Some(Prompt::Select {
            text: clean(line),
            items,
            all_label,
            enter_label,
        });
    }

    // press enter
    if lower.contains("press \"enter\"") || lower.contains("press enter") {
        let quit = lower.contains("quit");
        return Some(Prompt::Continue {
            text: clean(line).trim_end().to_string(),
            quit,
        });
    }

    // pacman / paru free-form selections
    if lower.starts_with("enter a selection")
        || lower.starts_with("enter a number")
        || lower.starts_with(":: packages to")
        || lower.starts_with("==> packages to")
        || lower.contains("or (1 2 3, 1-3, ^4)")
    {
        let default = line
            .rfind("(default=")
            .map(|p| line[p + 9..].trim_end_matches([')', ':', ' ']).to_string())
            .unwrap_or_default();
        return Some(Prompt::Input {
            text: clean(line),
            default,
        });
    }
    None
}

/// `3 - openssl 3.5.0-1 -> 3.5.1-1` → (3, "openssl …"). Also `[NEW]` markers are kept.
fn parse_numbered(t: &str) -> Option<(usize, String)> {
    let (n, rest) = t.split_once(" - ")?;
    let n: usize = n.trim().parse().ok()?;
    Some((n, rest.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(cursor: &str) -> Option<Prompt> {
        detect(&[], cursor)
    }

    #[test]
    fn yes_no_with_defaults() {
        assert_eq!(
            d("-> Proceed with update? [Y/n] "),
            Some(Prompt::YesNo {
                text: "Proceed with update?".into(),
                default_yes: true
            })
        );
        assert_eq!(
            d("-> Would you like to reboot now? [y/N]"),
            Some(Prompt::YesNo {
                text: "Would you like to reboot now?".into(),
                default_yes: false
            })
        );
        // pacman and paru
        assert!(matches!(
            d(":: Proceed with installation? [Y/n]"),
            Some(Prompt::YesNo {
                default_yes: true,
                ..
            })
        ));
        assert!(matches!(
            d(":: Proceed to review? [Y/n]:"),
            Some(Prompt::YesNo {
                default_yes: true,
                ..
            })
        ));
    }

    #[test]
    fn titles_and_verbs() {
        let p = d("-> Proceed with update? [Y/n] ").unwrap();
        assert_eq!(p.title(), "PROCEED WITH UPDATE");
        assert_eq!(p.verb(), "UPDATE");
        let p = d("-> Would you like to reboot now? [y/N]").unwrap();
        assert_eq!(p.verb(), "REBOOT NOW");
        assert!(p.dangerous());
        let p = d("-> Would you like to remove these orphan packages (and their potential dependencies) now? [y/N]").unwrap();
        assert_eq!(p.title(), "ORPHAN PACKAGES");
        assert_eq!(p.verb(), "REMOVE");
    }

    #[test]
    fn passwords() {
        assert_eq!(
            d("[sudo] password for kobago: "),
            Some(Prompt::Password {
                text: "[sudo] password for kobago:".into()
            })
        );
        assert!(matches!(d("Password: "), Some(Prompt::Password { .. })));
        assert!(matches!(
            d("doas (kobago@box) password:"),
            Some(Prompt::Password { .. })
        ));
        assert!(matches!(
            d("[sudo: authenticate] Password:"),
            Some(Prompt::Password { .. })
        ));
    }

    #[test]
    fn press_enter() {
        assert_eq!(
            d("==> Press \"enter\" to quit "),
            Some(Prompt::Continue {
                text: "Press \"enter\" to quit".into(),
                quit: true
            })
        );
        assert!(matches!(
            d("==> Press \"enter\" to continue "),
            Some(Prompt::Continue { quit: false, .. })
        ));
    }

    #[test]
    fn numbered_selection_collects_items() {
        let above: Vec<String> = [
            "==> Services:",
            "The following services require a post upgrade restart",
            "",
            "1 - systemd-journald.service",
            "2 - dbus-broker.service",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = detect(&above, "-> Select the service(s) to restart (e.g. 1 3 5), select 0 to restart them all or press \"enter\" to continue without restarting the service(s): ").unwrap();
        match p {
            Prompt::Select {
                items,
                all_label,
                enter_label,
                ..
            } => {
                assert_eq!(
                    items,
                    vec!["systemd-journald.service", "dbus-broker.service"]
                );
                assert_eq!(all_label, "RESTART ALL");
                assert_eq!(enter_label, "SKIP");
            }
            other => panic!("{other:?}"),
        }
        let above: Vec<String> = ["==> Arch News:", "1 - Something [NEW]", "2 - Other", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = detect(&above, "-> Select the news to read (e.g. 1 3 5), select 0 to read them all or press \"enter\" to proceed with update: ").unwrap();
        match p {
            Prompt::Select {
                items,
                all_label,
                enter_label,
                ..
            } => {
                assert_eq!(items.len(), 2);
                assert_eq!(all_label, "READ ALL");
                assert_eq!(enter_label, "PROCEED WITH UPDATE");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn selection_after_invalid_input_still_finds_the_list() {
        let above: Vec<String> = [
            "1 - a.service",
            "2 - b.service",
            "",
            "==> WARNING: Invalid input",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = detect(&above, "-> Select the service(s) to restart (e.g. 1 3 5), select 0 to restart them all or press \"enter\" to continue without restarting the service(s):").unwrap();
        assert!(matches!(p, Prompt::Select { ref items, .. } if items.len() == 2));
    }

    #[test]
    fn pacman_input_prompts() {
        assert_eq!(
            d("Enter a selection (default=all): "),
            Some(Prompt::Input {
                text: "Enter a selection (default=all):".into(),
                default: "all".into()
            })
        );
        assert!(matches!(
            d(":: Packages to exclude (eg: \"1 2 3\", \"1-3\", \"^4\" or repo name):"),
            Some(Prompt::Input { .. })
        ));
    }

    #[test]
    fn yay_prompts() {
        let p = d("==> Diffs to show? [N]one [A]ll [Ab]ort [I]nstalled [No]tInstalled or (1 2 3, 1-3, ^4) ").unwrap();
        assert!(matches!(p, Prompt::Input { .. }), "{p:?}");
        let p = d("==> Proceed to review? [Y/n]:").unwrap();
        assert_eq!(p.title(), "AUR HELPER");
        let p = d(":: Proceed with installation? [Y/n] ").unwrap();
        assert_eq!(p.title(), "PACMAN");
        assert_eq!(p.verb(), "INSTALL");
        let p = d(":: Do you want to remove these packages? [Y/n] ").unwrap();
        assert_eq!(p.title(), "PACMAN");
        assert!(matches!(
            d("[sudo] kobago のパスワード: "),
            Some(Prompt::Password { .. })
        ));
    }

    #[test]
    fn plain_output_is_not_a_prompt() {
        assert_eq!(d(""), None);
        assert_eq!(d("==> Looking for updates..."), None);
        assert_eq!(d("linux-cachyos 7.2.1-1 -> 7.2.2-1"), None);
        assert_eq!(d("Total Download Size:   12.34 MiB"), None);
    }
}
