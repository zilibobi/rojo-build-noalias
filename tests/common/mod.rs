#![allow(dead_code)]

pub mod model;

use std::path::{Path, PathBuf};
use std::process::Output;

pub fn rbn() -> assert_cmd::Command {
    assert_cmd::Command::new(env!("CARGO_BIN_EXE_rojo-build-noalias"))
}

/// Copy `tests/fixtures/<name>` into a fresh temp dir; returns the guard and the project dir.
pub fn fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join(name);

    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.unwrap();
        let rel = entry.path().strip_prefix(&src).unwrap();
        let to = dst.join(rel);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }

    (tmp, dst)
}

/// stdout then stderr, lossily decoded
pub fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The file's text, panicking with its path when it cannot be read.
pub fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// `--convert-to out` run from inside `project`, asserting it succeeds.
pub fn convert(project: &Path, out: &Path, extra: &[&str]) -> Output {
    let run = rbn()
        .current_dir(project)
        .args(["--convert-to", out.to_str().unwrap()])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "convert should succeed:\n{}",
        text(&run)
    );
    run
}

/// `rojo build <project> -o <model>` with the cwd in `cwd` (where the rokit manifest is),
/// asserting it succeeds, and the built model's roots.
pub fn rojo_build(cwd: &Path, project: &Path, model: &Path) -> Vec<model::Inst> {
    let run = std::process::Command::new("rojo")
        .current_dir(cwd)
        .arg("build")
        .arg(project)
        .arg("-o")
        .arg(model)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "rojo build of {} should succeed:\n{}",
        project.display(),
        text(&run)
    );
    model::load(model)
}

/// The JSON file at `p`, parsed.
pub fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("cannot parse {}: {e}", p.display()))
}

/// The temp dir [`build`] gives the binary for the project at `dir`: a `tmp` dir beside it,
/// inside the fixture's guard, so a sweep never reaches another run's workspace.
pub fn tmp_of(dir: &Path) -> PathBuf {
    let tmp = dir.parent().unwrap().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

/// A one-shot build run from inside `dir` with `--color never` and the temp dir set to
/// [`tmp_of`]`(dir)`.
pub fn build(dir: &Path, args: &[&str]) -> Output {
    let tmp = tmp_of(dir);

    rbn()
        .current_dir(dir)
        .env("TMPDIR", &tmp)
        .env("TEMP", &tmp)
        .env("TMP", &tmp)
        .args(["--color", "never"])
        .args(args)
        .output()
        .unwrap()
}

/// The `rojo-build-noalias.*` workspaces in `tmp`, sorted.
pub fn workspaces_in(tmp: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(tmp)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("rojo-build-noalias.")
        })
        .collect();
    found.sort();
    found
}

/// Sets a directory's mtime, which `File::open` cannot do on Windows without backup semantics.
pub fn set_dir_mtime(dir: &Path, time: std::time::SystemTime) {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS
        std::fs::OpenOptions::new()
            .access_mode(0x100)
            .custom_flags(0x0200_0000)
            .open(dir)
            .unwrap()
    };
    #[cfg(not(windows))]
    let file = std::fs::File::open(dir).unwrap();

    file.set_modified(time).unwrap();
}
