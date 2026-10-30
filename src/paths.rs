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
}
