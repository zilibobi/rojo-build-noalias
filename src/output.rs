use crate::log;
use anyhow::Context;
use std::path::Path;
use std::time::Duration;

/// How often a rename refused on Windows (Studio holding the output open) is retried.
const RETRIES: u32 = 10;
const RETRY_WAIT: Duration = Duration::from_millis(50);

/// Puts a copy of `built` at `dest` atomically (spec 7.3): the bytes go into a `.rbn-*` temp file
/// beside `dest`, which is then renamed over it, so a reader sees the old file or the new one and
/// never half of either. The output keeps the mode of the file it replaces (a new one takes the
/// built file's), not the temp file's owner-only mode. On Windows a refused rename is retried, then
/// the bytes are copied over `dest` with a warning.
pub fn swap(built: &Path, dest: &Path) -> anyhow::Result<()> {
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
}
