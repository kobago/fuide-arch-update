//! pacman / AUR helper queries and output parsing.
//!
//! Executables can be overridden for tests: `FUIDE_ARCH_PACMAN`, `FUIDE_ARCH_CHECKUPDATES`,
//! `FUIDE_ARCH_AUR_HELPER` (a path; `none` disables AUR support), `FUIDE_ARCH_PACCACHE`,
//! `FUIDE_ARCH_PKEXEC`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Reason {
    #[default]
    Unknown,
    Explicit,
    Dependency,
}

impl Reason {
    pub fn tag(self) -> &'static str {
        match self {
            Reason::Explicit => "EXPLICIT",
            Reason::Dependency => "DEP",
            Reason::Unknown => "--",
        }
    }
}

/// One package as described by `pacman -Qi` / `-Si` / `<helper> -Si`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub desc: String,
    /// `core`, `extra`, `cachyos-v3` … `aur` for AUR packages, `local` for other foreign ones.
    pub repo: String,
    pub url: String,
    pub licenses: Vec<String>,
    pub groups: Vec<String>,
    pub provides: Vec<String>,
    pub depends: Vec<String>,
    pub optdepends: Vec<String>,
    pub required_by: Vec<String>,
    pub conflicts: Vec<String>,
    pub installed_size_kib: Option<f64>,
    pub download_size_kib: Option<f64>,
    pub build_date: String,
    pub install_date: String,
    pub reason: Reason,
    pub packager: String,
    pub validated: String,
    /// True for `-Qi` records (the package is on this machine).
    pub installed: bool,
    /// Newer version known from the last check (`checkupdates` / `-Qua`).
    pub latest: Option<String>,
    /// No package requires it and it was installed as a dependency (`pacman -Qtdq`).
    pub orphan: bool,
    // AUR-only
    pub maintainer: String,
    pub votes: Option<u32>,
    pub popularity: Option<f64>,
    pub out_of_date: bool,
    pub last_modified: String,
}

impl Package {
    pub fn is_aur(&self) -> bool {
        self.repo == "aur"
    }
    pub fn is_foreign(&self) -> bool {
        self.repo == "aur" || self.repo == "local"
    }
    pub fn outdated(&self) -> bool {
        self.latest.is_some()
    }
}

/// A line of `pacman -Ss` / `<helper> -Ss`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchHit {
    pub repo: String,
    pub name: String,
    pub version: String,
    pub desc: String,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub votes: Option<u32>,
    pub popularity: Option<f64>,
    pub out_of_date: bool,
}

/// One pending update (`checkupdates` / `<helper> -Qua`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Upgrade {
    pub name: String,
    pub current: String,
    pub new: String,
    pub aur: bool,
}

// ------------------------------------------------------------------ executables

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

pub fn pacman_bin() -> PathBuf {
    env_path("FUIDE_ARCH_PACMAN", "pacman")
}

pub fn checkupdates_bin() -> PathBuf {
    env_path("FUIDE_ARCH_CHECKUPDATES", "checkupdates")
}

pub fn paccache_bin() -> PathBuf {
    env_path("FUIDE_ARCH_PACCACHE", "paccache")
}

fn on_path(cmd: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(cmd).is_file())
}

/// The AUR helper to use: `FUIDE_ARCH_AUR_HELPER` (path, or `none`), else the first of
/// `yay`, `paru`, `pikaur` on `PATH`.
pub fn aur_helper() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FUIDE_ARCH_AUR_HELPER") {
        return (v != "none" && !v.is_empty()).then(|| PathBuf::from(v));
    }
    ["yay", "paru", "pikaur"]
        .iter()
        .find(|h| on_path(h))
        .map(PathBuf::from)
}

/// `pkexec` (polkit: the desktop shows its own password dialog). `FUIDE_ARCH_PKEXEC`
/// overrides it (tests; `none` disables root commands).
pub fn privilege_cmd() -> Option<String> {
    if let Some(v) = std::env::var_os("FUIDE_ARCH_PKEXEC") {
        let v = v.to_string_lossy().into_owned();
        return (v != "none" && !v.is_empty()).then_some(v);
    }
    on_path("pkexec").then(|| "pkexec".to_string())
}

/// How an AUR helper is told to use `privilege` instead of `sudo`: yay and paru take
/// `--sudo <cmd>`; other helpers get nothing (they will use their own default).
pub fn helper_sudo_args(helper: &Path, privilege: &str) -> Vec<String> {
    let name = helper
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if name.contains("yay") || name.contains("paru") {
        vec!["--sudo".into(), privilege.into()]
    } else {
        Vec::new()
    }
}

pub fn has_checkupdates() -> bool {
    let b = checkupdates_bin();
    b.is_absolute() && b.is_file() || on_path(b.to_str().unwrap_or(""))
}

fn base(program: &Path) -> Command {
    let mut c = Command::new(program);
    c.env("LC_ALL", "C.UTF-8")
        .env_remove("LANGUAGE")
        .stdin(Stdio::null());
    c
}

/// Run and return stdout; `Err` carries stderr (or the spawn error).
fn run(cmd: &mut Command) -> Result<String, String> {
    let o = cmd
        .output()
        .map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    if o.status.success() {
        Ok(out)
    } else {
        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("exit code {}", o.status.code().unwrap_or(-1))
        } else {
            err
        })
    }
}

/// Like [`run`] but a non-zero exit with empty stderr is treated as "no results"
/// (`pacman -Ss` and `checkupdates` exit 1 / 2 when nothing matches).
fn run_lenient(cmd: &mut Command) -> Result<String, String> {
    match run(cmd) {
        Ok(s) => Ok(s),
        Err(e) if e.starts_with("exit code") => Ok(String::new()),
        Err(e) => Err(e),
    }
}

// ------------------------------------------------------------------ queries

/// Every installed package with its details (`pacman -Qi`), plus the foreign / orphan marks.
pub fn installed() -> Result<Vec<Package>, String> {
    let text = run(base(&pacman_bin()).args(["-Qi", "--color", "never"]))?;
    let mut pkgs = parse_info(&text, true);
    let foreign: HashSet<String> = run_lenient(base(&pacman_bin()).args(["-Qmq"]))?
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    let orphans: HashSet<String> = run_lenient(base(&pacman_bin()).args(["-Qtdq"]))?
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    let helper = aur_helper().is_some();
    // packages installed before pacman recorded "Installed From" say `None`: look the repo
    // up in the sync databases
    let sync = if pkgs.iter().any(|p| p.repo.is_empty() || p.repo == "None") {
        sync_list().unwrap_or_default()
    } else {
        HashMap::new()
    };
    for p in &mut pkgs {
        if foreign.contains(&p.name) {
            p.repo = if helper { "aur".into() } else { "local".into() };
        } else if p.repo.is_empty() || p.repo == "None" {
            p.repo = sync
                .get(&p.name)
                .map(|(repo, _)| repo.clone())
                .unwrap_or_else(|| "repo".into());
        }
        p.orphan = orphans.contains(&p.name);
    }
    Ok(pkgs)
}

/// Repo updates via `checkupdates` (pacman-contrib; network, no root).
pub fn check_repo() -> Result<Vec<Upgrade>, String> {
    let mut cmd = base(&checkupdates_bin());
    cmd.arg("--nocolor");
    let text = match run(&mut cmd) {
        Ok(s) => s,
        // exit 2 = no updates
        Err(e) if e == "exit code 2" => String::new(),
        Err(e) => return Err(e),
    };
    Ok(parse_upgrades(&text, false))
}

/// AUR updates via `<helper> -Qua` (network). `Ok(vec![])` when there is no helper.
pub fn check_aur() -> Result<Vec<Upgrade>, String> {
    let Some(helper) = aur_helper() else {
        return Ok(Vec::new());
    };
    let text = run_lenient(base(&helper).args(["-Qua", "--color", "never"]))?;
    Ok(parse_upgrades(&text, true))
}

/// Repo + AUR updates in one list.
pub fn check_all() -> Result<Vec<Upgrade>, String> {
    let mut v = check_repo()?;
    v.extend(check_aur()?);
    Ok(v)
}

/// `pacman -Ss <query>` (sync databases, no network).
pub fn search_repo(query: &str) -> Result<Vec<SearchHit>, String> {
    let text = run_lenient(base(&pacman_bin()).args(["-Ss", "--color", "never", query]))?;
    Ok(parse_search(&text))
}

/// `<helper> -Ss --aur <query>` (AUR RPC, network). Empty without a helper.
pub fn search_aur(query: &str) -> Result<Vec<SearchHit>, String> {
    let Some(helper) = aur_helper() else {
        return Ok(Vec::new());
    };
    let text = run_lenient(base(&helper).args(["-Ss", "--aur", "--color", "never", query]))?;
    Ok(parse_search(&text))
}

/// Details of not-installed repo packages (`pacman -Si`).
pub fn info_repo(names: &[String]) -> Result<Vec<Package>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let text = run_lenient(
        base(&pacman_bin())
            .args(["-Si", "--color", "never"])
            .args(names),
    )?;
    Ok(parse_info(&text, false))
}

/// Details of AUR packages (`<helper> -Si`, network).
pub fn info_aur(names: &[String]) -> Result<Vec<Package>, String> {
    let Some(helper) = aur_helper() else {
        return Ok(Vec::new());
    };
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let text = run_lenient(
        base(&helper)
            .args(["-Si", "--aur", "--color", "never"])
            .args(names),
    )?;
    let mut pkgs = parse_info(&text, false);
    for p in &mut pkgs {
        if p.repo.is_empty() {
            p.repo = "aur".into();
        }
    }
    Ok(pkgs)
}

/// `pacman -Sl`: (repo, name) → version, for the repo of packages whose `-Qi` lacks it.
pub fn sync_list() -> Result<HashMap<String, (String, String)>, String> {
    let text = run_lenient(base(&pacman_bin()).args(["-Sl", "--color", "never"]))?;
    let mut out = HashMap::new();
    for l in text.lines() {
        let mut it = l.split_whitespace();
        if let (Some(repo), Some(name), Some(ver)) = (it.next(), it.next(), it.next()) {
            out.entry(name.to_string())
                .or_insert((repo.to_string(), ver.to_string()));
        }
    }
    Ok(out)
}

/// Size of the package cache in KiB (`du -sk`; unreadable download dirs are ignored).
pub fn cache_size_kib() -> Option<f64> {
    let o = Command::new("du")
        .args(["-sk", "/var/cache/pacman/pkg"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `paccache -dk<n>` candidates for pruning (dry run, no root).
pub fn cache_candidates(keep: u32) -> Option<u32> {
    let o = base(&paccache_bin())
        .arg(format!("-dk{keep}"))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    for l in text.lines() {
        if let Some(p) = l.find(" candidate") {
            let head = &l[..p];
            if let Some(n) = head.rsplit([' ', ':']).next() {
                if let Ok(n) = n.trim().parse() {
                    return Some(n);
                }
            }
        }
    }
    Some(0)
}

/// Newest modification time of the sync databases (`/var/lib/pacman/sync/*.db`), unix time.
pub fn last_sync_epoch() -> Option<u64> {
    let dir = std::env::var_os("FUIDE_ARCH_SYNC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/pacman/sync"));
    let mut newest = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "db") {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                let t = t
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                newest = Some(newest.map_or(t, |n: u64| n.max(t)));
            }
        }
    }
    newest
}

/// Running kernel (`uname -r`) and whether its modules directory still has a kernel image
/// (if not, a kernel update is waiting for a reboot).
pub fn kernel() -> (String, bool) {
    let running = Command::new("uname")
        .arg("-r")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let reboot =
        !running.is_empty() && !Path::new(&format!("/usr/lib/modules/{running}/vmlinuz")).is_file();
    (running, reboot)
}

// ------------------------------------------------------------------ parsing (pure)

fn parse_size_kib(s: &str) -> Option<f64> {
    let mut it = s.split_whitespace();
    let v: f64 = it.next()?.parse().ok()?;
    let unit = it.next().unwrap_or("B");
    let f = match unit {
        "B" => 1.0 / 1024.0,
        "KiB" => 1.0,
        "MiB" => 1024.0,
        "GiB" => 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    Some(v * f)
}

fn list(v: &str) -> Vec<String> {
    if v == "None" {
        return Vec::new();
    }
    v.split("  ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Parse `Key : Value` blocks (`pacman -Qi` / `-Si`, `yay -Si`). Continuation lines are
/// indented; blocks are separated by blank lines. `installed` marks `-Qi` output.
pub fn parse_info(text: &str, installed: bool) -> Vec<Package> {
    let mut out = Vec::new();
    let mut fields: Vec<(String, String)> = Vec::new();
    let flush = |fields: &mut Vec<(String, String)>, out: &mut Vec<Package>| {
        if fields.is_empty() {
            return;
        }
        let mut p = Package {
            installed,
            ..Default::default()
        };
        for (k, v) in fields.drain(..) {
            let v = v.trim().to_string();
            match k.as_str() {
                "Name" => p.name = v,
                "Version" => p.version = v,
                "Description" => p.desc = if v == "None" { String::new() } else { v },
                "Repository" | "Installed From" => p.repo = v,
                "URL" => p.url = if v == "None" { String::new() } else { v },
                "Licenses" => p.licenses = list(&v),
                "Groups" => p.groups = list(&v),
                "Provides" => p.provides = list(&v),
                "Depends On" => p.depends = list(&v),
                "Optional Deps" => p.optdepends = list(&v),
                "Required By" => p.required_by = list(&v),
                "Conflicts With" => p.conflicts = list(&v),
                "Installed Size" => p.installed_size_kib = parse_size_kib(&v),
                "Download Size" => p.download_size_kib = parse_size_kib(&v),
                "Build Date" => p.build_date = v,
                "Install Date" => p.install_date = v,
                "Install Reason" => {
                    p.reason = if v.starts_with("Explicitly") {
                        Reason::Explicit
                    } else if v.starts_with("Installed as a dependency") {
                        Reason::Dependency
                    } else {
                        Reason::Unknown
                    }
                }
                "Packager" | "Maintainer" if k == "Packager" => p.packager = v,
                "Maintainer" => p.maintainer = v,
                "Validated By" => p.validated = v,
                "Votes" => p.votes = v.parse().ok(),
                "Popularity" => p.popularity = v.parse().ok(),
                "Out-of-date" => p.out_of_date = v != "No",
                "Last Modified" => p.last_modified = v,
                _ => {}
            }
        }
        if !p.name.is_empty() {
            out.push(p);
        }
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut fields, &mut out);
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // continuation of the previous field (multi-line dependency lists)
            if let Some((_, v)) = fields.last_mut() {
                v.push_str("  ");
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            // AUR helpers pad keys wider than pacman; keys never contain ':'
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    flush(&mut fields, &mut out);
    out
}

/// Parse `pacman -Ss` / `<helper> -Ss` output:
/// `repo/name version [installed]` (or `[installed: v]`, AUR: `(+votes popularity) [Out-of-date: …]`)
/// followed by an indented description line.
pub fn parse_search(text: &str) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::new();
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(h) = out.last_mut() {
                if h.desc.is_empty() {
                    h.desc = line.trim().to_string();
                }
            }
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(id), Some(version)) = (it.next(), it.next()) else {
            continue;
        };
        let Some((repo, name)) = id.split_once('/') else {
            continue;
        };
        let rest: Vec<&str> = it.collect();
        let mut h = SearchHit {
            repo: repo.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            ..Default::default()
        };
        let mut i = 0;
        while i < rest.len() {
            let t = rest[i];
            if t.starts_with("[installed") {
                h.installed = true;
                if t == "[installed:" {
                    if let Some(v) = rest.get(i + 1) {
                        h.installed_version = Some(v.trim_end_matches(']').to_string());
                        i += 1;
                    }
                }
            } else if let Some(v) = t.strip_prefix("(+") {
                h.votes = v.parse().ok();
                if let Some(p) = rest.get(i + 1) {
                    h.popularity = p.trim_end_matches(')').parse().ok();
                    i += 1;
                }
            } else if t.contains("Out-of-date") {
                h.out_of_date = true;
            }
            i += 1;
        }
        out.push(h);
    }
    out
}

/// `name current -> new` lines (`checkupdates`, `<helper> -Qua`).
pub fn parse_upgrades(text: &str, aur: bool) -> Vec<Upgrade> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let current = it.next()?;
            if it.next()? != "->" {
                return None;
            }
            let new = it.next()?;
            Some(Upgrade {
                name: name.to_string(),
                current: current.to_string(),
                new: new.to_string(),
                aur,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QI: &str = "Installed From  : cachyos-v3
Name            : bash
Version         : 5.3.15-2
Description     : The GNU Bourne Again shell
Architecture    : x86_64_v3
URL             : https://www.gnu.org/software/bash/bash.html
Licenses        : GPL-3.0-or-later
Groups          : None
Provides        : sh
Depends On      : readline  libreadline.so=8-64  glibc  ncurses
Optional Deps   : bash-completion: for tab completion [installed]
Required By     : alsa-utils  base
Optional For    : mdadm
Conflicts With  : None
Replaces        : None
Installed Size  : 9.68 MiB
Packager        : CachyOS <admin@cachyos.org>
Build Date      : Mon Jun 29 01:28:21 2026
Install Date    : Mon Jun 29 17:15:09 2026
Install Reason  : Installed as a dependency for another package
Install Script  : Yes
Validated By    : Signature

Name            : yay
Version         : 13.0.1-1
Description     : Yet another yogurt.
Install Reason  : Explicitly installed
Installed Size  : 8.00 MiB

";

    #[test]
    fn parses_qi_blocks() {
        let p = parse_info(QI, true);
        assert_eq!(p.len(), 2);
        let b = &p[0];
        assert_eq!(b.name, "bash");
        assert_eq!(b.repo, "cachyos-v3");
        assert_eq!(
            b.depends,
            vec!["readline", "libreadline.so=8-64", "glibc", "ncurses"]
        );
        assert_eq!(b.groups, Vec::<String>::new());
        assert_eq!(b.required_by, vec!["alsa-utils", "base"]);
        assert_eq!(b.reason, Reason::Dependency);
        assert!((b.installed_size_kib.unwrap() - 9.68 * 1024.0).abs() < 0.01);
        assert!(b.installed);
        assert_eq!(p[1].reason, Reason::Explicit);
        assert_eq!(p[1].repo, "");
    }

    #[test]
    fn parses_aur_si_with_continuations() {
        let text = "Repository                    : aur
Name                          : visual-studio-code-bin
Version                       : 1.136.1-1
Description                   : Visual Studio Code (vscode): Editor
URL                           : https://code.visualstudio.com/
Depends On                    : libxkbfile  gnupg  gtk3
                                gcc-libs  libnotify
Maintainer                    : dcelasun
Popularity                    : 23.867764
Votes                         : 1706
Out-of-date                   : No
";
        let p = parse_info(text, false);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].repo, "aur");
        assert_eq!(p[0].depends.len(), 5);
        assert_eq!(p[0].votes, Some(1706));
        assert_eq!(p[0].maintainer, "dcelasun");
        assert!(!p[0].out_of_date);
        assert_eq!(p[0].desc, "Visual Studio Code (vscode): Editor");
    }

    #[test]
    fn parses_search_output() {
        let text = "cachyos-v3/bash 5.3.15-2 [installed]
    The GNU Bourne Again shell
core/bash 5.3.15-1 [installed: 5.3.15-2]
    The GNU Bourne Again shell
aur/pikaur-static 1.33.3-1 (+2 0.24) [182d17h]
    AUR helper without dependencies
aur/old-thing 1.0-1 (+0 0.00) [Out-of-date: 2025-01-01]
    stale
extra/ripgrep 15.2.0-1
    A search tool
";
        let h = parse_search(text);
        assert_eq!(h.len(), 5);
        assert!(h[0].installed && h[0].installed_version.is_none());
        assert_eq!(h[1].installed_version.as_deref(), Some("5.3.15-2"));
        assert_eq!(h[2].repo, "aur");
        assert_eq!(h[2].votes, Some(2));
        assert_eq!(h[2].popularity, Some(0.24));
        assert_eq!(h[2].desc, "AUR helper without dependencies");
        assert!(h[3].out_of_date);
        assert!(!h[4].installed);
        assert_eq!(h[4].name, "ripgrep");
    }

    #[test]
    fn parses_upgrade_lines() {
        let u = parse_upgrades(
            "linux-cachyos 7.2.1-1 -> 7.2.2-1\n  yay 13.0.0-1 -> 13.0.1-1\ngarbage\n",
            true,
        );
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].name, "linux-cachyos");
        assert_eq!(u[1].new, "13.0.1-1");
        assert!(u[1].aur);
    }

    #[test]
    fn helper_sudo_flag() {
        assert_eq!(
            helper_sudo_args(Path::new("/usr/bin/yay"), "pkexec"),
            vec!["--sudo", "pkexec"]
        );
        assert_eq!(
            helper_sudo_args(Path::new("paru"), "pkexec"),
            vec!["--sudo", "pkexec"]
        );
        assert!(helper_sudo_args(Path::new("pikaur"), "pkexec").is_empty());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size_kib("1330.50 KiB"), Some(1330.5));
        assert_eq!(parse_size_kib("2.00 GiB"), Some(2.0 * 1024.0 * 1024.0));
        assert_eq!(parse_size_kib("None"), None);
    }
}
