//! One GUI per session. The first instance takes a lock in the runtime directory and listens on
//! a Unix socket next to it; a later launch (the tray's click, a second menu click) hands its
//! start-up request to that socket and exits, and the running window comes to the front.
//!
//! Wire format: one line per connection — `show`, `upgrade`, or `select NAME`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use crate::app::StartUp;

/// What a second launch asks the running instance to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Show,
    Upgrade,
    Select(String),
}

impl Request {
    pub fn from_start(start: &StartUp) -> Self {
        if start.upgrade {
            Self::Upgrade
        } else if let Some(name) = &start.select {
            Self::Select(name.clone())
        } else {
            Self::Show
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Show => "show\n".into(),
            Self::Upgrade => "upgrade\n".into(),
            Self::Select(name) => format!("select {name}\n"),
        }
    }

    fn decode(line: &str) -> Option<Self> {
        let line = line.trim();
        match line.split_once(' ') {
            None if line == "show" => Some(Self::Show),
            None if line == "upgrade" => Some(Self::Upgrade),
            Some(("select", name)) if !name.is_empty() => Some(Self::Select(name.into())),
            _ => None,
        }
    }
}

/// Outcome of `claim`.
pub enum Claim {
    /// This process is the instance: keep the guard alive for as long as the window lives.
    Primary(Guard),
    /// Another instance took the request; exit.
    Forwarded,
}

/// Lock + socket of the running instance. Dropping it removes the socket file.
pub struct Guard {
    _lock: std::fs::File,
    listener: Option<UnixListener>,
    socket: PathBuf,
    rx: Option<Receiver<Request>>,
}

/// Unix socket addresses are short (108 bytes): a long runtime dir falls back to the temp dir,
/// keyed by the lock path so different runtime dirs still get different sockets.
fn socket_path(dir: &Path) -> PathBuf {
    let socket = dir.join("fuide-arch-update.sock");
    if socket.as_os_str().len() < 100 {
        return socket;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    std::env::temp_dir().join(format!("fuide-arch-update-{:016x}.sock", h.finish()))
}

/// `$FUIDE_ARCH_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR`, else the temp dir.
pub fn runtime_dir() -> PathBuf {
    for var in ["FUIDE_ARCH_RUNTIME_DIR", "XDG_RUNTIME_DIR"] {
        if let Some(d) = std::env::var_os(var).filter(|d| !d.is_empty()) {
            return PathBuf::from(d);
        }
    }
    std::env::temp_dir()
}

/// Become the instance, or hand `request` to the one that is running. When the lock is taken
/// but the socket does not answer, run anyway: a window beats nothing.
pub fn claim(dir: &Path, request: &Request) -> Claim {
    let lock_path = dir.join("fuide-arch-update.lock");
    let socket = socket_path(dir);
    let lock = match try_lock(&lock_path) {
        Some(f) => f,
        None => {
            if forward(&socket, request).is_ok() {
                return Claim::Forwarded;
            }
            eprintln!(
                "fuide-arch-update: another instance holds {} but does not answer on {}; starting anyway",
                lock_path.display(),
                socket.display()
            );
            return Claim::Primary(Guard {
                _lock: std::fs::File::open("/dev/null").expect("/dev/null"),
                listener: None,
                socket: PathBuf::new(),
                rx: None,
            });
        }
    };
    // the socket file of a previous instance that died without cleaning up
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => Some(l),
        Err(e) => {
            eprintln!(
                "fuide-arch-update: cannot listen on {}: {e}; later launches open new windows",
                socket.display()
            );
            None
        }
    };
    Claim::Primary(Guard {
        _lock: lock,
        listener,
        socket,
        rx: None,
    })
}

impl Guard {
    /// Start accepting requests; each one wakes the UI through `ctx`.
    pub fn serve(&mut self, ctx: egui::Context) {
        let Some(listener) = self.listener.take() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::Builder::new()
            .name("fuide-arch-update-instance".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else { break };
                    let mut line = String::new();
                    let _ = BufReader::new(conn).read_line(&mut line);
                    if let Some(req) = Request::decode(&line) {
                        if tx.send(req).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            })
            .expect("spawn instance thread");
    }

    /// Requests that arrived since the last call.
    pub fn drain(&mut self) -> Vec<Request> {
        match &self.rx {
            Some(rx) => rx.try_iter().collect(),
            None => Vec::new(),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.socket.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

fn try_lock(path: &Path) -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
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

fn forward(socket: &Path, request: &Request) -> std::io::Result<()> {
    let mut s = UnixStream::connect(socket)?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    s.write_all(request.encode().as_bytes())?;
    s.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        for r in [
            Request::Show,
            Request::Upgrade,
            Request::Select("linux-zen".into()),
        ] {
            assert_eq!(Request::decode(&r.encode()), Some(r));
        }
        assert_eq!(Request::decode("select \n"), None);
        assert_eq!(Request::decode("dance\n"), None);
        assert_eq!(
            Request::from_start(&StartUp {
                upgrade: false,
                select: Some("yay".into())
            }),
            Request::Select("yay".into())
        );
        assert_eq!(
            Request::from_start(&StartUp {
                upgrade: true,
                select: Some("yay".into())
            }),
            Request::Upgrade
        );
        assert_eq!(Request::from_start(&StartUp::default()), Request::Show);
    }

    #[test]
    fn second_claim_forwards_to_the_first() {
        round_trip(std::env::temp_dir().join(format!("fau-inst-{}", std::process::id())));
    }

    #[test]
    fn long_runtime_dir_uses_a_short_socket() {
        let dir = std::env::temp_dir().join(format!(
            "fau-inst-long-{}-{}",
            std::process::id(),
            "x".repeat(120)
        ));
        assert!(dir.as_os_str().len() > 108);
        assert!(socket_path(&dir).as_os_str().len() < 108);
        assert_ne!(socket_path(&dir), socket_path(&dir.join("other")));
        round_trip(dir);
    }

    fn round_trip(dir: PathBuf) {
        std::fs::create_dir_all(&dir).unwrap();
        let mut guard = match claim(&dir, &Request::Show) {
            Claim::Primary(g) => g,
            Claim::Forwarded => panic!("nothing was running"),
        };
        guard.serve(egui::Context::default());
        assert!(matches!(
            claim(&dir, &Request::Select("linux".into())),
            Claim::Forwarded
        ));
        assert!(matches!(claim(&dir, &Request::Upgrade), Claim::Forwarded));
        // the accept thread delivers asynchronously
        let mut got = Vec::new();
        for _ in 0..200 {
            got.extend(guard.drain());
            if got.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, [Request::Select("linux".into()), Request::Upgrade]);
        let socket = socket_path(&dir);
        assert!(socket.exists());
        drop(guard);
        assert!(!socket.exists(), "socket removed on exit");
        // the lock is free again: a fresh claim becomes primary
        assert!(matches!(claim(&dir, &Request::Show), Claim::Primary(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
