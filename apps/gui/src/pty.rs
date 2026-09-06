//! Run pacman / an AUR helper (any command) inside a pseudo-terminal.
//!
//! The script expects a real terminal: `read -rp` prompts, `sudo` asking on `/dev/tty`,
//! `column -t` and pacman's progress bars all want one. The child gets the pty slave as its
//! controlling terminal (`setsid` + `TIOCSCTTY`); the master is read on a thread that hands
//! raw bytes to the UI (`Msg::Output`) and reports the exit status (`Msg::Exit`).

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

use nix::pty::Winsize;

pub enum Msg {
    Output(Vec<u8>),
    Exit {
        label: String,
        #[allow(dead_code)]
        args: Vec<String>,
        code: Option<i32>,
        signal: Option<i32>,
        elapsed_secs: f32,
    },
}

/// What is currently running.
#[derive(Clone, Debug)]
pub struct Job {
    pub label: String,
    #[allow(dead_code)]
    pub args: Vec<String>,
    pub started: Instant,
}

pub struct Runner {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    job: Option<Job>,
    /// Write end (a dup of the pty master) while a job runs.
    master: Option<File>,
    pid: Option<i32>,
    cols: u16,
    rows: u16,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            job: None,
            master: None,
            pid: None,
            cols: 100,
            rows: 40,
        }
    }

    pub fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// Feed a message as if the reader thread had produced it (tests).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn inject(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    pub fn poll(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            if let Msg::Exit { .. } = &m {
                self.job = None;
                self.master = None;
                self.pid = None;
            }
            out.push(m);
        }
        out
    }

    /// Start `program <args>` in a fresh pty. `english` runs it under `LC_ALL=C.UTF-8` so the
    /// prompts are the English ones the dialogs recognise. Returns false if a job is already
    /// running.
    pub fn run_program(
        &mut self,
        program: PathBuf,
        label: String,
        args: Vec<String>,
        english: bool,
        ctx: egui::Context,
    ) -> bool {
        if self.job.is_some() {
            return false;
        }
        let ws = Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = match nix::pty::openpty(Some(&ws), None) {
            Ok(p) => p,
            Err(e) => {
                self.report_spawn_failure(label, args, format!("openpty failed: {e}"), &ctx);
                return false;
            }
        };
        let slave: OwnedFd = pty.slave;
        let master: OwnedFd = pty.master;
        // close-on-exec: another child spawned meanwhile (or by another thread) must not
        // inherit this pty, or its slave stays open and the reader never sees EOF
        for fd in [&slave, &master] {
            // SAFETY: fcntl on fds we own
            unsafe {
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .env("TERM", "xterm-256color")
            .env_remove("COLUMNS")
            .env_remove("LINES")
            .stdin(Stdio::from(match slave.try_clone() {
                Ok(fd) => fd,
                Err(e) => {
                    self.report_spawn_failure(label, args, format!("dup failed: {e}"), &ctx);
                    return false;
                }
            }))
            .stdout(Stdio::from(match slave.try_clone() {
                Ok(fd) => fd,
                Err(e) => {
                    self.report_spawn_failure(label, args, format!("dup failed: {e}"), &ctx);
                    return false;
                }
            }))
            .stderr(Stdio::from(slave));
        if english {
            cmd.env("LC_ALL", "C.UTF-8").env("LANGUAGE", "");
        }
        // SAFETY: only async-signal-safe calls (setsid, ioctl) between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.report_spawn_failure(
                    label,
                    args,
                    format!("cannot start {}: {e}", program.display()),
                    &ctx,
                );
                return false;
            }
        };
        // the parent's copies of the slave are dropped with `cmd`; keep a dup of the master to write to
        let writer = match master.try_clone() {
            Ok(fd) => File::from(fd),
            Err(e) => {
                let _ = child.kill();
                self.report_spawn_failure(label, args, format!("dup failed: {e}"), &ctx);
                return false;
            }
        };
        self.pid = Some(child.id() as i32);
        self.master = Some(writer);
        self.job = Some(Job {
            label: label.clone(),
            args: args.clone(),
            started: Instant::now(),
        });
        drop(cmd);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            // SAFETY: `master` is an open fd we own; File takes it over.
            let mut reader = unsafe { File::from_raw_fd(master.as_raw_fd()) };
            std::mem::forget(master);
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tx.send(Msg::Output(buf[..n].to_vec()));
                        ctx.request_repaint();
                    }
                    // EIO: the slave side is closed (child gone) — normal end on Linux
                    Err(_) => break,
                }
            }
            let status = child.wait();
            let (code, signal) = match status {
                Ok(s) => {
                    use std::os::unix::process::ExitStatusExt;
                    (s.code(), s.signal())
                }
                Err(_) => (None, None),
            };
            let _ = tx.send(Msg::Exit {
                label,
                args,
                code,
                signal,
                elapsed_secs: t0.elapsed().as_secs_f32(),
            });
            ctx.request_repaint();
        });
        true
    }

    fn report_spawn_failure(
        &self,
        label: String,
        args: Vec<String>,
        why: String,
        ctx: &egui::Context,
    ) {
        let _ = self.tx.send(Msg::Output(
            format!("\x1b[31m==> ERROR:\x1b[0m {why}\r\n").into_bytes(),
        ));
        let _ = self.tx.send(Msg::Exit {
            label,
            args,
            code: None,
            signal: None,
            elapsed_secs: 0.0,
        });
        ctx.request_repaint();
    }

    /// Send text to the child's terminal (what typing would do). Appends nothing.
    pub fn write(&mut self, text: &str) -> bool {
        match &mut self.master {
            Some(m) => m.write_all(text.as_bytes()).and_then(|_| m.flush()).is_ok(),
            None => false,
        }
    }

    /// Send a line: `text` plus Enter.
    pub fn send_line(&mut self, text: &str) -> bool {
        let mut s = text.to_string();
        s.push('\n');
        self.write(&s)
    }

    /// Ctrl+C through the line discipline: SIGINT to the foreground process group.
    pub fn interrupt(&mut self) -> bool {
        self.write("\x03")
    }

    /// SIGTERM to the whole process group (last resort).
    pub fn terminate(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: plain syscall on a pid we spawned
            unsafe {
                libc::killpg(pid, libc::SIGTERM);
            }
        }
    }

    /// Tell the child its terminal size (`column -t`, pacman bars). Cheap to call every frame:
    /// it only issues the ioctl when the size changed.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.clamp(40, 500);
        let rows = rows.clamp(8, 200);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        if let Some(m) = &self.master {
            let ws = Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: TIOCSWINSZ with a valid winsize on an open pty master
            unsafe {
                libc::ioctl(m.as_raw_fd(), libc::TIOCSWINSZ as _, &ws as *const Winsize);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const BASH: &str = "/bin/bash";
    /// (exit code, signal) of a finished child.
    type Exit = Option<(Option<i32>, Option<i32>)>;

    fn drain(r: &mut Runner, until: impl Fn(&[u8], &Exit) -> bool) -> (Vec<u8>, Exit) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = Vec::new();
        let mut exit = None;
        while Instant::now() < deadline {
            for m in r.poll() {
                match m {
                    Msg::Output(b) => out.extend(b),
                    Msg::Exit { code, signal, .. } => exit = Some((code, signal)),
                }
            }
            if until(&out, &exit) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        (out, exit)
    }

    #[test]
    fn runs_a_script_in_a_pty_and_answers_a_prompt() {
        let ctx = egui::Context::default();
        let mut r = Runner::new();
        r.resize(60, 20);
        let script = r#"[ -t 0 ] && echo "tty ok $(tput cols)"; read -rp "-> Proceed? [Y/n] " a; echo "got=$a"; exit 3"#;
        assert!(r.run_program(
            BASH.into(),
            "test".into(),
            vec!["-c".into(), script.into()],
            true,
            ctx
        ));
        assert!(r.running());
        let (out, _) = drain(&mut r, |o, _| String::from_utf8_lossy(o).contains("[Y/n]"));
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(
            text.contains("tty ok 60"),
            "child must see the pty: {text:?}"
        );
        assert!(r.send_line("y"));
        let (out, exit) = drain(&mut r, |_, e| e.is_some());
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(text.contains("got=y"), "{text:?}");
        assert_eq!(exit, Some((Some(3), None)));
        assert!(!r.running());
    }

    #[test]
    fn interrupt_stops_the_child() {
        let ctx = egui::Context::default();
        let mut r = Runner::new();
        assert!(r.run_program(
            BASH.into(),
            "sleep".into(),
            vec!["-c".into(), "echo up; sleep 30".into()],
            true,
            ctx
        ));
        let _ = drain(&mut r, |o, _| String::from_utf8_lossy(o).contains("up"));
        assert!(r.interrupt());
        let (_, exit) = drain(&mut r, |_, e| e.is_some());
        let (code, signal) = exit.expect("child should exit after Ctrl+C");
        assert!(
            signal == Some(2) || code.is_some_and(|c| c != 0),
            "code={code:?} signal={signal:?}"
        );
    }

    #[test]
    fn only_one_job_at_a_time() {
        let ctx = egui::Context::default();
        let mut r = Runner::new();
        assert!(r.run_program(
            BASH.into(),
            "a".into(),
            vec!["-c".into(), "sleep 0.2".into()],
            true,
            ctx.clone()
        ));
        assert!(!r.run_program(
            BASH.into(),
            "b".into(),
            vec!["-c".into(), "true".into()],
            true,
            ctx
        ));
        let _ = drain(&mut r, |_, e| e.is_some());
    }
}
