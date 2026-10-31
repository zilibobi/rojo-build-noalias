use crate::cli::ColorChoice;
use crate::{log, shutdown, sourcemap};
use anyhow::Context;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

/// How this run invokes the `rojo` binary. `cwd` is the original project directory, so a rokit
/// shim or mise finds its manifest.
#[derive(Clone, Debug)]
pub struct Rojo {
    pub cwd: PathBuf,
    pub color: ColorChoice,
    pub verbose: u8,
}

impl Rojo {
    /// `rojo sourcemap --absolute <project>`. On a non-zero exit the error's first line is
    /// v1.2.4's message with Rojo's exit code, and the lines after it are Rojo's stderr.
    pub fn sourcemap(&self, project: &Path) -> anyhow::Result<sourcemap::Node> {
        let mut cmd = Command::new("rojo");
        cmd.arg("sourcemap")
            .arg("--absolute")
            .arg(project)
            .current_dir(&self.cwd);

        let out = run(cmd, Pipes::Capture)
            .with_context(|| format!("cannot run rojo in {}", self.cwd.display()))?;

        if !out.status.success() {
            let code = out
                .status
                .code()
                .map_or_else(|| "none".to_string(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&out.stderr);

            anyhow::bail!(
                "Rojo sourcemap generation failed for {} with exit code {code}\n{}",
                project.display(),
                stderr.trim_end()
            );
        }

        let json = String::from_utf8_lossy(&out.stdout);

        sourcemap::parse(&json)
            .with_context(|| format!("Rojo's sourcemap for {} did not parse", project.display()))
    }
}

/// Where `rojo build` writes: a model file, or the Studio plugins folder under a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildOut {
    File(PathBuf),
    Plugin(String),
}

impl Rojo {
    /// `rojo build <project>` into `out`, with `--color` and `-v` passed on. Rojo's stdout and
    /// stderr lines are forwarded with the `ROJO` prefix, except its `Built project to` line,
    /// which the caller replaces with its own marker once the output is in place. A non-zero exit
    /// is an `Err`. While it runs, [`kill_running`] can stop it.
    pub fn build(&self, project: &Path, out: &BuildOut) -> anyhow::Result<()> {
        let mut cmd = Command::new("rojo");
        cmd.arg("build").arg(project);

        match out {
            BuildOut::File(path) => cmd.arg("-o").arg(path),
            BuildOut::Plugin(name) => cmd.arg("--plugin").arg(name),
        };

        let color = match self.color {
            ColorChoice::Auto => "auto",
            ColorChoice::Always => "always",
            ColorChoice::Never => "never",
        };
        cmd.arg("--color").arg(color);

        for _ in 0..self.verbose {
            cmd.arg("-v");
        }

        cmd.current_dir(&self.cwd);

        let status = run(cmd, Pipes::Forward)
            .with_context(|| format!("cannot run rojo in {}", self.cwd.display()))?
            .status;

        if !status.success() {
            let code = status
                .code()
                .map_or_else(|| "none".to_string(), |c| c.to_string());
            anyhow::bail!("Rojo build failed with exit code {code}");
        }

        Ok(())
    }
}

/// A running child and whether it has been reaped, which only happens under its lock.
struct Running {
    child: Child,
    reaped: bool,
    /// the program run, so the tests can pick out their own children
    program: OsString,
}

type Shared = Arc<Mutex<Running>>;

/// The rojo children running now, so a shutdown can kill them while another thread waits on them.
static RUNNING: Mutex<Vec<Shared>> = Mutex::new(Vec::new());

fn registry() -> MutexGuard<'static, Vec<Shared>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock(child: &Shared) -> MutexGuard<'_, Running> {
    child.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many rojo children are running.
pub fn running() -> usize {
    registry().len()
}

#[cfg(test)]
fn running_program(program: &str) -> usize {
    registry()
        .iter()
        .filter(|c| lock(c).program == program)
        .count()
}

/// Kills every rojo child running now, with everything it started, and returns how many there
/// were. `rojo` on PATH is often a rokit or mise shim that runs the real Rojo as its own child,
/// which holds the pipes open, so killing the shim alone would not end the run. A watch registers
/// this with `shutdown::on_request`; it is safe to call from any thread.
pub fn kill_running() -> usize {
    kill_where(|_| true)
}

/// Kills every running rojo child on the first shutdown request, from whichever thread makes it.
pub fn kill_on_shutdown() {
    shutdown::on_request(|| {
        kill_running();
    });
}

fn kill_where(pick: impl Fn(&OsStr) -> bool) -> usize {
    let children = registry().clone();
    let mut killed = 0;

    for child in &children {
        let mut running = lock(child);

        if !pick(&running.program) {
            continue;
        }

        killed += 1;

        // a child not yet reaped still owns its pid, so the group cannot belong to anyone else
        if !running.reaped {
            kill_tree(running.child.id());
            let _ = running.child.kill();
        }
    }

    killed
}

/// Puts the child in a process group of its own, which [`kill_tree`] kills as one.
#[cfg(unix)]
fn own_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_: &mut Command) {}

/// Kills the process group led by `pid` (std has no killpg and `libc` is not a dependency, so
/// this asks the `kill` utility). procps-ng 4 refuses `-s KILL -- -<pgid>` and takes
/// `-KILL -- -<pgid>` (measured); the POSIX spelling is the fallback for other `kill`s.
#[cfg(unix)]
fn kill_tree(pid: u32) {
    let group = format!("-{pid}");
    let short = ["-KILL", "--", group.as_str()];
    let posix = ["-s", "KILL", "--", group.as_str()];
    let spellings: [&[&str]; 2] = [&short, &posix];

    for args in spellings {
        let killed = Command::new("kill")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());

        if killed {
            return;
        }
    }
}

/// Kills `pid` and its descendants. Not exercised by the tests, which run on unix only; Windows
/// has no process groups to signal, and `taskkill /T` walks the tree instead of a job object,
/// which would need a Windows API crate.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn kill_tree(_: u32) {}

/// Removes its child from [`RUNNING`] however the run ends.
struct Registered(Shared);

impl Drop for Registered {
    fn drop(&mut self) {
        registry().retain(|c| !Arc::ptr_eq(c, &self.0));
    }
}

/// What to do with a child's output.
#[derive(Clone, Copy)]
enum Pipes {
    /// forward each line with the `ROJO` prefix, dropping Rojo's marker
    Forward,
    /// keep the bytes for the caller
    Capture,
}

struct Finished {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Spawns `cmd` in a process group of its own with piped output, drains both pipes and waits for
/// it. The child is listed in [`RUNNING`] meanwhile; its lock is taken only once both pipes have
/// closed, so a kill never waits on a child that is still writing.
fn run(mut cmd: Command, pipes: Pipes) -> anyhow::Result<Finished> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_group(&mut cmd);

    let program = cmd.get_program().to_os_string();
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let shared = Arc::new(Mutex::new(Running {
        child,
        reaped: false,
        program,
    }));
    registry().push(shared.clone());
    let _registered = Registered(shared.clone());

    let errors = stderr.map(|pipe| std::thread::spawn(move || drain(pipe, pipes)));
    let stdout = stdout.map(|pipe| drain(pipe, pipes)).unwrap_or_default();
    let stderr = errors
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();

    let mut running = lock(&shared);
    let status = running.child.wait();
    running.reaped = true;

    Ok(Finished {
        status: status?,
        stdout,
        stderr,
    })
}

fn drain(mut pipe: impl Read, pipes: Pipes) -> Vec<u8> {
    let mut bytes = Vec::new();

    match pipes {
        Pipes::Forward => forward(pipe),
        Pipes::Capture => {
            let _ = pipe.read_to_end(&mut bytes);
        }
    }

    bytes
}

fn forward(pipe: impl Read) {
    for line in BufReader::new(pipe).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches('\r');

        if !line.contains("Built project to") {
            log::rojo(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_sourcemap_carries_the_exit_code_and_stderr() {
        let rojo = Rojo {
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            color: ColorChoice::Never,
            verbose: 0,
        };
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("default.project.json");
        std::fs::write(&project, r#"{"name": "X", "tree": {"$path": "missing"}}"#).unwrap();

        let err = rojo.sourcemap(&project).unwrap_err().to_string();
        let mut lines = err.lines();
        assert_eq!(
            lines.next(),
            Some(
                format!(
                    "Rojo sourcemap generation failed for {} with exit code 1",
                    project.display()
                )
                .as_str()
            )
        );
        assert!(lines.next().is_some(), "{err}");
    }

    // the registry is global: the kill tests pick out their own children by program, and run one
    // at a time
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn here() -> Rojo {
        Rojo {
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            color: ColorChoice::Never,
            verbose: 0,
        }
    }

    #[test]
    fn build_writes_the_model_and_fails_with_the_exit_code() {
        let _serial = serial();
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("default.project.json");
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/init.luau"), "return 1").unwrap();
        std::fs::write(&project, r#"{"name": "X", "tree": {"$path": "src"}}"#).unwrap();

        let out = tmp.path().join("out/build.rbxmx");
        std::fs::create_dir(tmp.path().join("out")).unwrap();
        here()
            .build(&project, &BuildOut::File(out.clone()))
            .unwrap();
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .starts_with("<roblox")
        );

        std::fs::write(&project, r#"{"name": "X", "tree": {"$path": "missing"}}"#).unwrap();
        let err = here()
            .build(&project, &BuildOut::File(out))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Rojo build failed with exit code 1");
    }

    #[cfg(unix)]
    #[test]
    fn kill_running_stops_a_running_child() {
        let _serial = serial();
        let child = std::thread::spawn(|| {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            run(cmd, Pipes::Forward).map(|f| f.status)
        });

        let started = std::time::Instant::now();

        while running_program("sleep") == 0 {
            assert!(started.elapsed().as_secs() < 10, "the child never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(kill_where(|p| p == "sleep"), 1);
        let status = child.join().unwrap().unwrap();
        assert!(!status.success());
        assert!(started.elapsed().as_secs() < 10);
        assert_eq!(running_program("sleep"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn kill_running_reaches_a_shim_grandchild() {
        // a rokit shim runs the real rojo as its child, which holds the pipes open
        let _serial = serial();
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());

        let child = std::thread::spawn(move || {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(script);
            run(cmd, Pipes::Forward).map(|f| f.status)
        });

        let started = std::time::Instant::now();

        while running_program("sh") == 0
            || std::fs::read_to_string(&pid_file).map_or(true, |p| !p.ends_with('\n'))
        {
            assert!(started.elapsed().as_secs() < 10, "the child never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let grandchild = std::fs::read_to_string(&pid_file).unwrap();
        let grandchild = grandchild.trim();

        assert_eq!(kill_where(|p| p == "sh"), 1);
        let status = child.join().unwrap().unwrap();
        assert!(!status.success());
        assert!(started.elapsed().as_secs() < 10, "the kill was not prompt");

        // the grandchild is gone too, once init has reaped it
        let alive = || {
            Command::new("kill")
                .arg("-0")
                .arg(grandchild)
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };

        while alive() {
            assert!(
                started.elapsed().as_secs() < 10,
                "the grandchild survived the kill"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
