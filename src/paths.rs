use std::path::{Component, Path, PathBuf};

/// Directory names never copied, mirrored or scanned, at any depth.
pub const SKIP_NAMES: [&str; 4] = [".git", "node_modules", ".luau-lsp", ".vscode"];

/// Lexical normalization: drops `.` and trailing separators, folds `..` against a preceding name.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }

    out
}

/// `path` with links resolved and, on Windows, short names made long and no `\\?\` prefix,
/// when it exists; else `path` as it is.
pub fn resolved(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with its nearest existing ancestor [`resolved`] and the rest joined on as given, so a
/// directory that does not exist yet is spelled as the existing part of it is.
pub fn resolved_existing(path: &Path) -> PathBuf {
    for dir in path.ancestors() {
        if dir.as_os_str().is_empty() {
            break;
        }

        if let Ok(real) = dunce::canonicalize(dir) {
            let rest = path.strip_prefix(dir).unwrap_or(Path::new(""));

            return if rest.as_os_str().is_empty() {
                real
            } else {
                real.join(rest)
            };
        }
    }

    path.to_path_buf()
}

/// `path` with its parent directory [`resolved`], so a file that does not exist yet (an output)
/// is spelled as its directory is.
pub fn resolved_parent(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => resolved(dir).join(name),
        _ => path.to_path_buf(),
    }
}

pub fn is_script(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("lua" | "luau")
    )
}

pub fn is_init(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("init.lua" | "init.luau")
    )
}

/// Whether any component of `rel` is one of [`SKIP_NAMES`].
pub fn skipped(rel: &Path) -> bool {
    rel.components().any(|c| match c {
        Component::Normal(name) => SKIP_NAMES.iter().any(|s| name == *s),
        _ => false,
    })
}

/// The path's components joined by `/` whatever the platform separator.
pub fn slash(path: &Path) -> String {
    let mut out = String::new();

    for comp in path.components() {
        match comp {
            Component::RootDir => out.push('/'),
            Component::Prefix(p) => out.push_str(&p.as_os_str().to_string_lossy()),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }

                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_joins_with_forward_slashes() {
        assert_eq!(
            slash(&Path::new("a").join("My Folder").join("b.luau")),
            "a/My Folder/b.luau"
        );
        assert_eq!(slash(Path::new("/p/src")), "/p/src");
        assert_eq!(slash(Path::new("")), "");
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(
            normalize(Path::new("/p/./src/../src/mod/")),
            Path::new("/p/src/mod")
        );
        assert_eq!(normalize(Path::new("/p/../..")), Path::new("/"));
        assert_eq!(normalize(Path::new("../a/./b/..")), Path::new("../a"));
    }

    #[test]
    fn skip_names_match_whole_components() {
        assert!(skipped(Path::new("pkg/x/.git/HEAD")));
        assert!(skipped(Path::new("node_modules")));
        assert!(!skipped(Path::new("pkg/gitx/a")));
        assert!(!skipped(Path::new("pkg/.gitignore")));
    }

    #[test]
    fn script_and_init_predicates() {
        assert!(is_script(Path::new("a/b.luau")));
        assert!(is_script(Path::new("a/b.lua")));
        assert!(!is_script(Path::new("a/b.json")));
        assert!(!is_script(Path::new("a/luau")));
        assert!(is_init(Path::new("/p/init.luau")));
        assert!(is_init(Path::new("init.lua")));
        assert!(!is_init(Path::new("/p/reinit.luau")));
        assert!(!is_init(Path::new("/p/init.json")));
    }

    #[cfg(unix)]
    #[test]
    fn resolved_spellings_follow_links_in_the_existing_part() {
        let tmp = tempfile::tempdir().unwrap();
        let real = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(real.join("d")).unwrap();
        std::os::unix::fs::symlink(real.join("d"), real.join("l")).unwrap();
        let link = tmp.path().join("l");

        assert_eq!(resolved(&link), real.join("d"));
        assert_eq!(resolved(&link.join("gone")), link.join("gone"));
        assert_eq!(
            resolved_parent(&link.join("out.rbxm")),
            real.join("d/out.rbxm")
        );
        assert_eq!(resolved_existing(&link.join("a/b")), real.join("d/a/b"));
        assert_eq!(resolved_existing(&link), real.join("d"));
    }
}
