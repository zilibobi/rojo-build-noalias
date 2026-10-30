use crate::paths::SKIP_NAMES;
use anyhow::Context;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PREFIX: &str = "rojo-build-noalias.";

/// A workspace younger than this is never swept: its run may not have taken the lock yet.
const STALE_AGE: Duration = Duration::from_secs(10);

/// A per-run temp dir holding `tree/`, `out/` and `lock`. The run holds the lock for its
/// lifetime; dropping the workspace removes the dir.
pub struct Workspace {
    // closed before the dir is removed, which Windows needs
    _lock: File,
    dir: tempfile::TempDir,
}

impl Workspace {
    /// Sweeps dead runs' workspaces from the system temp dir, then creates one there.
    pub fn create() -> anyhow::Result<Workspace> {
        let temp_root = std::env::temp_dir();
        sweep_stale(&temp_root);
        Workspace::create_in(&temp_root)
    }

    pub fn create_in(temp_root: &Path) -> anyhow::Result<Workspace> {
        let base = dunce::canonicalize(temp_root)
            .with_context(|| format!("cannot open the temp directory {}", temp_root.display()))?;
        let dir = tempfile::Builder::new()
            .prefix(PREFIX)
            .tempdir_in(&base)
            .with_context(|| format!("cannot create a workspace in {}", base.display()))?;

        // the lock first, so a sweep never sees this dir without one for long
        let lock =
            File::create(dir.path().join("lock")).context("cannot create the workspace lock")?;
        lock.try_lock()
            .map_err(|e| anyhow::anyhow!("cannot lock the workspace: {e}"))?;

        std::fs::create_dir(dir.path().join("tree")).context("cannot create the workspace")?;
        std::fs::create_dir(dir.path().join("out")).context("cannot create the workspace")?;

        Ok(Workspace { _lock: lock, dir })
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn tree(&self) -> PathBuf {
        self.root().join("tree")
    }

    pub fn out(&self) -> PathBuf {
        self.root().join("out")
    }
}

/// Removes every `rojo-build-noalias.*` dir in `temp_root` that is older than a few seconds and
/// whose lock can be taken (its run is dead). Returns how many were removed.
pub fn sweep_stale(temp_root: &Path) -> usize {
    sweep(temp_root, STALE_AGE)
}

fn sweep(temp_root: &Path, min_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(temp_root) else {
        return 0;
    };

    let mut removed = 0;

    for entry in entries.flatten() {
        let name = entry.file_name();

        if !name.to_string_lossy().starts_with(PREFIX) {
            continue;
        }

        let dir = entry.path();

        if is_dead(&dir, min_age) && std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }

    removed
}

/// Old enough and its lock is free. The lock is released again before this returns, since
/// Windows cannot remove a file that is still open.
fn is_dead(dir: &Path, min_age: Duration) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };

    if !meta.is_dir() {
        return false;
    }

    // a clock that went backwards reads as fresh
    let old = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age >= min_age);

    if !old {
        return false;
    }

    let Ok(lock) = File::open(dir.join("lock")) else {
        return false;
    };

    lock.try_lock().is_ok()
}

/// Copies `from` into `to`, following symlinks, skipping [`SKIP_NAMES`] at any depth and the
/// `exclude` path. Symlink loops and unreadable entries are skipped; a failed write is an error.
pub fn copy_tree(from: &Path, to: &Path, exclude: Option<&Path>) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("cannot create {}", to.display()))?;

    let walk = walkdir::WalkDir::new(from)
        .follow_links(true)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            let skipped = e.depth() > 0 && SKIP_NAMES.iter().any(|n| e.file_name() == *n);
            !skipped && exclude != Some(e.path())
        });

    // a loop or an unreadable entry is an Err here and is skipped, as v1.2.4 skipped them
    for entry in walk.flatten() {
        let Ok(rel) = entry.path().strip_prefix(from) else {
            continue;
        };

        let dest = to.join(rel);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("cannot create {}", dest.display()))?;
            continue;
        }

        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };

        std::fs::write(&dest, bytes).with_context(|| format!("cannot write {}", dest.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn workspaces(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(PREFIX))
            .collect();
        names.sort();
        names
    }

    fn fake_run(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("tree")).unwrap();
        std::fs::write(dir.join("lock"), "").unwrap();
        dir
    }

    #[test]
    fn create_then_drop_removes_the_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(tmp.path()).unwrap();
        let root = ws.root().to_path_buf();

        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(PREFIX)
        );
        assert!(ws.tree().is_dir());
        assert!(ws.out().is_dir());
        assert!(root.join("lock").is_file());
        assert_eq!(ws.tree(), root.join("tree"));

        drop(ws);
        assert!(!root.exists());
    }

    #[test]
    fn a_live_workspace_cannot_be_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::create_in(tmp.path()).unwrap();
        assert_eq!(sweep(tmp.path(), Duration::ZERO), 0);
        assert!(ws.tree().is_dir());
    }

    #[test]
    fn sweep_removes_unlocked_and_keeps_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let dead = fake_run(tmp.path(), "rojo-build-noalias.dead");
        let live = fake_run(tmp.path(), "rojo-build-noalias.live");
        let other = fake_run(tmp.path(), "something-else");

        let held = File::open(live.join("lock")).unwrap();
        held.lock().unwrap();

        assert_eq!(sweep(tmp.path(), Duration::ZERO), 1);
        assert!(!dead.exists());
        assert!(live.join("tree").is_dir());
        assert!(other.exists());
        assert_eq!(workspaces(tmp.path()), vec!["rojo-build-noalias.live"]);
    }

    #[test]
    fn sweep_spares_a_fresh_unlocked_dir() {
        // a run that has created its dir but not yet taken the lock
        let tmp = tempfile::tempdir().unwrap();
        let fresh = fake_run(tmp.path(), "rojo-build-noalias.fresh");
        std::fs::remove_file(fresh.join("lock")).unwrap();
        let young = fake_run(tmp.path(), "rojo-build-noalias.young");

        assert_eq!(sweep_stale(tmp.path()), 0);
        assert!(fresh.exists());
        assert!(young.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_stale_removes_an_old_dead_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dead = fake_run(tmp.path(), "rojo-build-noalias.old");
        let old = SystemTime::now() - Duration::from_secs(60);
        File::open(&dead).unwrap().set_modified(old).unwrap();

        assert_eq!(sweep_stale(tmp.path()), 1);
        assert!(!dead.exists());
    }

    #[test]
    fn copy_tree_skips_names() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        for name in SKIP_NAMES {
            std::fs::create_dir_all(src.join("pkg").join(name)).unwrap();
            std::fs::write(src.join("pkg").join(name).join("x.luau"), "x").unwrap();
        }

        std::fs::create_dir_all(src.join("pkg/gitx")).unwrap();
        std::fs::write(src.join("pkg/gitx/a.luau"), "a").unwrap();
        std::fs::write(src.join("pkg/.gitignore"), "g").unwrap();
        std::fs::create_dir_all(src.join("out/deep")).unwrap();
        std::fs::write(src.join("out/deep/o.luau"), "o").unwrap();

        copy_tree(&src, &dst, Some(&src.join("out"))).unwrap();

        for name in SKIP_NAMES {
            assert!(!dst.join("pkg").join(name).exists(), "{name} was copied");
        }

        assert_eq!(std::fs::read(dst.join("pkg/gitx/a.luau")).unwrap(), b"a");
        assert_eq!(std::fs::read(dst.join("pkg/.gitignore")).unwrap(), b"g");
        assert!(!dst.join("out").exists());
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_survives_a_symlink_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::write(src.join("a/f.luau"), "f").unwrap();
        std::os::unix::fs::symlink("..", src.join("a/loop")).unwrap();
        std::fs::write(src.join("real.luau"), "r").unwrap();
        std::os::unix::fs::symlink("real.luau", src.join("alias.luau")).unwrap();

        copy_tree(&src, &dst, None).unwrap();

        assert_eq!(std::fs::read(dst.join("a/f.luau")).unwrap(), b"f");
        assert_eq!(std::fs::read(dst.join("alias.luau")).unwrap(), b"r");
        assert!(!dst.join("a/loop/a").exists());
    }
}
