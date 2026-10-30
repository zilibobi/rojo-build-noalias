use crate::cli::ColorChoice;
use crate::{log, sourcemap};
use anyhow::Context;
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
        let out = Command::new("rojo")
            .arg("sourcemap")
            .arg("--absolute")
            .arg(project)
            .current_dir(&self.cwd)
            .output()
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

        let status = run_forwarded(cmd)
            .with_context(|| format!("cannot run rojo in {}", self.cwd.display()))?;

        if !status.success() {
            let code = status
                .code()
                .map_or_else(|| "none".to_string(), |c| c.to_string());
            anyhow::bail!("Rojo build failed with exit code {code}");
        }

        Ok(())
    }
}

type Shared = Arc<Mutex<Child>>;

/// The rojo children running now, so a shutdown can kill them while another thread waits on them.
static RUNNING: Mutex<Vec<Shared>> = Mutex::new(Vec::new());

fn registry() -> MutexGuard<'static, Vec<Shared>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock(child: &Shared) -> MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many rojo children are running.
pub fn running() -> usize {
    registry().len()
}

/// Kills every rojo child running now and returns how many there were. A watch registers it with
/// `shutdown::on_request`, so a shutdown during a build does not wait for Rojo; it is safe to call
/// from any thread.
pub fn kill_running() -> usize {
    let children = registry().clone();

    for child in &children {
        let _ = lock(child).kill();
    }

    children.len()
}

/// Removes its child from [`RUNNING`] however the run ends.
struct Registered(Shared);

impl Drop for Registered {
    fn drop(&mut self) {
        registry().retain(|c| !Arc::ptr_eq(c, &self.0));
    }
}

/// Spawns `cmd` with piped output, forwards its lines, and waits for it. The child is listed in
/// [`RUNNING`] meanwhile; its lock is taken only once both pipes have closed, so a kill never
/// waits on a child that is still writing.
fn run_forwarded(mut cmd: Command) -> anyhow::Result<ExitStatus> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let shared = Arc::new(Mutex::new(child));
    registry().push(shared.clone());
    let _registered = Registered(shared.clone());

    let errors = stderr.map(|pipe| std::thread::spawn(move || forward(pipe)));

    if let Some(pipe) = stdout {
        forward(pipe);
    }

    if let Some(thread) = errors {
        let _ = thread.join();
    }

    let status = lock(&shared).wait()?;

    Ok(status)
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

    // the registry is global, so a kill must not reach another test's build
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
            run_forwarded(cmd)
        });

        let started = std::time::Instant::now();

        while running() == 0 {
            assert!(started.elapsed().as_secs() < 10, "the child never started");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(kill_running(), 1);
        let status = child.join().unwrap().unwrap();
        assert!(!status.success());
        assert!(started.elapsed().as_secs() < 10);
        assert_eq!(running(), 0);
    }
}
