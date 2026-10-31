use super::{build, model, text, tmp_of};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const MARKER: &str = "OK   Built project to ";

/// Names each fresh build's output apart, so two oracles never share a file.
static FRESH: AtomicUsize = AtomicUsize::new(0);

/// A `--watch` run of the binary on a fixture copy, with its stdout read line by line by a thread.
/// The temp dir is [`tmp_of`] the project, so two watchers on one project share it.
pub struct Watcher {
    child: Child,
    /// `cat` between the child's stdout and the reader (unix), so the pipe can be closed while
    /// the reader is blocked on it: killing `cat` closes the child's read end at once
    relay: Option<Child>,
    lines: Receiver<String>,
    seen: RefCell<Vec<String>>,
    /// lines before this one have been counted by `wait_built` or `count_built`
    cursor: Cell<usize>,
    dir: PathBuf,
    out: PathBuf,
    tmp: PathBuf,
    extra: Vec<String>,
}

impl Watcher {
    /// `--color never --watch -o <tmp>/out.rbxm <extra>` from inside `dir`.
    pub fn spawn(dir: &Path, extra: &[&str]) -> Watcher {
        let out = tmp_of(dir).join("out.rbxm");
        Watcher::spawn_out(dir, &out, extra)
    }

    /// As [`Watcher::spawn`], building to `out`.
    pub fn spawn_out(dir: &Path, out: &Path, extra: &[&str]) -> Watcher {
        Watcher::spawn_with(dir, out, extra, &[])
    }

    /// As [`Watcher::spawn_out`], with extra environment variables.
    pub fn spawn_with(dir: &Path, out: &Path, extra: &[&str], env: &[(&str, &str)]) -> Watcher {
        let tmp = tmp_of(dir);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rojo-build-noalias"));
        cmd.current_dir(dir)
            .env("TMPDIR", &tmp)
            .env("TEMP", &tmp)
            .env("TMP", &tmp)
            .args(["--color", "never", "--watch", "-o"])
            .arg(out)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (relay, reader) = relay(stdout);
        let (tx, lines) = channel();

        std::thread::spawn(move || {
            use std::io::BufRead;

            for line in std::io::BufReader::new(reader).lines() {
                let Ok(line) = line else {
                    return;
                };

                if tx.send(line).is_err() {
                    return;
                }
            }
        });

        Watcher {
            child,
            relay,
            lines,
            seen: RefCell::new(Vec::new()),
            cursor: Cell::new(0),
            dir: dir.to_path_buf(),
            out: out.to_path_buf(),
            tmp,
            extra: extra.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Takes every line that has arrived, without waiting.
    fn drain(&self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.borrow_mut().push(line);
        }
    }

    /// Waits for one more line until `deadline`; false once it passes or the pipe has closed.
    fn next_line(&self, deadline: Instant) -> bool {
        let left = deadline.saturating_duration_since(Instant::now());

        match self.lines.recv_timeout(left) {
            Ok(line) => {
                self.seen.borrow_mut().push(line);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => {
                std::thread::sleep(left);
                false
            }
        }
    }

    /// Whether a marker line arrives within `within`, counting from the last counted line. The
    /// marker found is counted.
    pub fn wait_built(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;

        loop {
            self.drain();

            let found = self.seen.borrow()[self.cursor.get()..]
                .iter()
                .position(|l| l.starts_with(MARKER));

            if let Some(i) = found {
                self.cursor.set(self.cursor.get() + i + 1);
                return true;
            }

            if !self.next_line(deadline) && Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Collects lines for `during`, then counts the markers since the last counted line.
    pub fn count_built(&self, during: Duration) -> usize {
        let deadline = Instant::now() + during;

        while Instant::now() < deadline {
            self.next_line(deadline);
        }

        self.drain();
        let seen = self.seen.borrow();
        let count = seen[self.cursor.get()..]
            .iter()
            .filter(|l| l.starts_with(MARKER))
            .count();
        self.cursor.set(seen.len());
        count
    }

    /// Whether any line so far contains `needle`.
    pub fn saw(&self, needle: &str) -> bool {
        self.drain();
        self.seen.borrow().iter().any(|l| l.contains(needle))
    }

    /// Every line so far, joined, for assertion messages.
    pub fn log(&self) -> String {
        self.drain();
        self.seen.borrow().join("\n")
    }

    pub fn model(&self) -> Vec<model::Inst> {
        model::load(&self.out)
    }

    pub fn out_bytes(&self) -> Vec<u8> {
        std::fs::read(&self.out)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", self.out.display()))
    }

    /// The model's root with `path` below it, panicking with the log when it is missing.
    pub fn find(&self, path: &[&str]) -> model::Inst {
        let roots = self.model();
        roots[0]
            .find(path)
            .cloned()
            .unwrap_or_else(|| panic!("no {path:?} in the model:\n{:#?}", roots[0].shape()))
    }

    /// Whether the model holds the instance at `path` below its root.
    pub fn has(&self, path: &[&str]) -> bool {
        self.model()[0].find(path).is_some()
    }

    /// No Source in the model still holds one of the fixtures' aliases.
    pub fn assert_no_raw_aliases(&self) {
        for (path, source) in model::sources(&self.model()) {
            for alias in ["\"@mod/", "\"@pkg/", "\"@vfx"] {
                assert!(!source.contains(alias), "{path} holds {alias}:\n{source}");
            }
        }
    }

    /// The watcher has settled (no marker for 600 ms) and its output equals a fresh one-shot
    /// build of the same tree, byte for byte. The fresh build goes to another file, or onto the
    /// watcher's own output when that lies inside the project, since a fresh build to another
    /// file would then mount the watcher's output.
    pub fn assert_matches_fresh_build(&self) {
        assert_eq!(
            self.count_built(Duration::from_millis(600)),
            0,
            "the watcher was still building:\n{}",
            self.log()
        );

        let watched = self.out_bytes();
        let n = FRESH.fetch_add(1, Ordering::SeqCst);
        let fresh = if self.out.starts_with(&self.dir) {
            self.out.clone()
        } else {
            self.tmp.join(format!("fresh-{n}.rbxm"))
        };

        let mut args = vec!["-o", fresh.to_str().unwrap()];
        args.extend(self.extra.iter().map(String::as_str));
        let run = build(&self.dir, &args);
        assert!(run.status.success(), "fresh build failed:\n{}", text(&run));

        let fresh_bytes = std::fs::read(&fresh).unwrap();

        if watched != fresh_bytes {
            let shape = |bytes: &[u8]| {
                let file = self.tmp.join(format!("cmp-{n}.rbxm"));
                std::fs::write(&file, bytes).unwrap();
                format!("{:#?}", model::load(&file))
            };

            panic!(
                "the watch output differs from a fresh build\nwatch: {}\nfresh: {}\nlog:\n{}",
                shape(&watched),
                shape(&fresh_bytes),
                self.log()
            );
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn tmp(&self) -> &Path {
        &self.tmp
    }

    /// Waits up to `within` for the process to exit.
    pub fn wait_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;

        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }

            if Instant::now() >= deadline {
                return None;
            }

            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Sends SIGTERM and waits for the exit (10 s at most).
    #[cfg(unix)]
    pub fn term(&mut self) -> ExitStatus {
        signal(self.pid(), "-TERM");
        self.wait_exit(Duration::from_secs(10))
            .expect("the watcher did not exit within 10 s of SIGTERM")
    }

    /// Closes the read end of the child's stdout, so its next write fails with EPIPE.
    #[cfg(unix)]
    pub fn close_stdout(&mut self) {
        let mut relay = self.relay.take().expect("stdout is still open");
        let _ = relay.kill();
        let _ = relay.wait();
    }
}

/// Sends `sig` (`-TERM`, `-INT`) to `pid` with the `kill` utility.
#[cfg(unix)]
pub fn signal(pid: u32, sig: &str) {
    let ok = Command::new("kill")
        .arg(sig)
        .arg(pid.to_string())
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill {sig} {pid} failed");
}

#[cfg(unix)]
fn relay(stdout: ChildStdout) -> (Option<Child>, Box<dyn std::io::Read + Send>) {
    let mut cat = Command::new("cat")
        .stdin(Stdio::from(stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = cat.stdout.take().unwrap();
    (Some(cat), Box::new(out))
}

#[cfg(not(unix))]
fn relay(stdout: ChildStdout) -> (Option<Child>, Box<dyn std::io::Read + Send>) {
    (None, Box::new(stdout))
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("watcher log:\n{}", self.log());
        }

        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .arg("-TERM")
                    .arg(self.pid().to_string())
                    .status();
            }

            if self.wait_exit(Duration::from_secs(5)).is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        if let Some(mut relay) = self.relay.take() {
            let _ = relay.kill();
            let _ = relay.wait();
        }
    }
}
