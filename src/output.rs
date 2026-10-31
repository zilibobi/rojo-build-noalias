use crate::log;
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often a rename refused on Windows (Studio holding the output open) is retried.
const RETRIES: u32 = 10;
const RETRY_WAIT: Duration = Duration::from_millis(50);

/// Puts a copy of `built` at `dest` atomically (spec 7.3): the bytes go into a `.rbn-*` temp file
/// beside `dest`, which is then renamed over it, so a reader sees the old file or the new one and
/// never half of either. An output that is a symlink is written through: the swap happens at the
/// file it points to, beside that file, and the link stays. The output keeps the mode of the file it replaces (a new one takes the
/// built file's), not the temp file's owner-only mode. On Windows a refused rename is retried, then
/// the bytes are copied over `dest` with a warning.
pub fn swap(built: &Path, dest: &Path) -> anyhow::Result<()> {
    let target = through_links(dest)?;
    let dest = target.as_path();

    let dir = match dest.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };

    let mut tmp = tempfile::NamedTempFile::with_prefix_in(".rbn-", dir)
        .with_context(|| format!("cannot write {}", dest.display()))?;
    let mut src = std::fs::File::open(built)
        .with_context(|| format!("cannot read the built model {}", built.display()))?;
    std::io::copy(&mut src, tmp.as_file_mut())
        .with_context(|| format!("cannot write {}", tmp.path().display()))?;

    let mode = std::fs::metadata(dest)
        .or_else(|_| src.metadata())
        .map(|m| m.permissions());

    if let Ok(mode) = mode {
        tmp.as_file()
            .set_permissions(mode)
            .with_context(|| format!("cannot write {}", tmp.path().display()))?;
    }

    let mut tries = 0;

    loop {
        match tmp.persist(dest) {
            Ok(_) => return Ok(()),
            Err(e) if cfg!(windows) && tries < RETRIES => {
                tries += 1;
                tmp = e.file;
                std::thread::sleep(RETRY_WAIT);
            }
            Err(e) if cfg!(windows) => {
                std::fs::copy(built, dest)
                    .with_context(|| format!("cannot write {}", dest.display()))?;
                log::warn(&format!(
                    "could not replace {} atomically; copied over it",
                    dest.display()
                ));
                drop(e.file);
                return Ok(());
            }
            Err(e) => {
                return Err(e.error).with_context(|| format!("cannot replace {}", dest.display()));
            }
        }
    }
}

/// How many links [`through_links`] follows before giving up, as Linux's `MAXSYMLINKS`.
const MAX_LINKS: usize = 40;

/// `path` with every symlink at its end followed, even to a target that does not exist yet.
pub(crate) fn through_links(path: &Path) -> anyhow::Result<PathBuf> {
    let mut path = path.to_path_buf();

    for _ in 0..MAX_LINKS {
        let is_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());

        if !is_link {
            return Ok(path);
        }

        let to = std::fs::read_link(&path)
            .with_context(|| format!("cannot read the link {}", path.display()))?;
        path = match path.parent() {
            Some(dir) => dir.join(to),
            None => to,
        };
    }

    anyhow::bail!("too many levels of symlinks at {}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".rbn-"))
            .collect()
    }

    #[test]
    fn swap_replaces_and_leaves_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("build.rbxm");
        let out = tmp.path().join("site");
        std::fs::create_dir(&out).unwrap();
        let dest = out.join("out.rbxm");

        std::fs::write(&built, "first").unwrap();
        swap(&built, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");

        std::fs::write(&built, "second, longer").unwrap();
        swap(&built, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"second, longer");
        assert_eq!(std::fs::read(&built).unwrap(), b"second, longer");
        assert!(temp_names(&out).is_empty(), "{:?}", temp_names(&out));
    }

    #[test]
    fn swap_into_a_missing_dir_fails_and_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("build.rbxm");
        std::fs::write(&built, "x").unwrap();

        let err = swap(&built, &tmp.path().join("gone/out.rbxm")).unwrap_err();
        assert!(format!("{err:#}").contains("gone"), "{err:#}");
        assert!(!tmp.path().join("gone").exists());
    }

    #[cfg(unix)]
    #[test]
    fn swap_keeps_the_mode_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("build.rbxm");
        std::fs::write(&built, "x").unwrap();
        std::fs::set_permissions(&built, std::fs::Permissions::from_mode(0o644)).unwrap();

        // a new output takes the built file's mode, not the temp file's 0600
        let dest = tmp.path().join("out.rbxm");
        swap(&built, &dest).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dest), 0o644);

        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o664)).unwrap();
        swap(&built, &dest).unwrap();
        assert_eq!(mode(&dest), 0o664);
    }

    #[cfg(unix)]
    #[test]
    fn swap_writes_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let built = tmp.path().join("build.rbxm");
        let real_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("links");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::create_dir(&link_dir).unwrap();
        let target = real_dir.join("out.rbxm");
        std::fs::write(&target, "old").unwrap();
        let link = link_dir.join("out.rbxm");
        std::os::unix::fs::symlink("../real/out.rbxm", &link).unwrap();

        std::fs::write(&built, "new").unwrap();
        swap(&built, &link).unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(temp_names(&real_dir).is_empty());
        assert!(temp_names(&link_dir).is_empty());

        // a dangling link gets its target created
        std::fs::remove_file(&target).unwrap();
        swap(&built, &link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }
}
