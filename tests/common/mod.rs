#![allow(dead_code)]

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
