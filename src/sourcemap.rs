use crate::log;
use crate::paths::normalize;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub name: String,
    pub class_name: String,
    #[serde(default)]
    pub file_paths: Vec<PathBuf>,
    #[serde(default)]
    pub children: Vec<Node>,
}

pub fn parse(json: &str) -> anyhow::Result<Node> {
    Ok(serde_json::from_str(json)?)
}

/// `util.getProjectJsonPath`: the node's first `*.project.json` file path.
fn project_json_path(node: &Node) -> Option<&Path> {
    node.file_paths
        .iter()
        .find(|p| p.to_string_lossy().ends_with(".project.json"))
        .map(PathBuf::as_path)
}

/// v1's `string.find(last, "init.luau$") or string.find(last, "init.lua$")`: an unanchored Lua
/// pattern whose `.` matches any byte, so `reinit.luau` counts too. Kept for parity.
fn v1_init_suffix(name: &str) -> bool {
    let b = name.as_bytes();
    let ends = |tail: &[u8]| {
        b.len() >= tail.len() + 5
            && b.ends_with(tail)
            && b[..b.len() - tail.len() - 1].ends_with(b"init")
    };

    ends(b"luau") || ends(b"lua")
}

struct Found<'a> {
    sub: Option<&'a Node>,
    // v1's `depth`, which a failed recursion overwrites with nil
    depth: Option<usize>,
    is_project: bool,
}

/// `util.tryGetFolderPath` (v1.2.4 `util.luau:205-274`) with the same depth bookkeeping.
fn try_folder<'a>(child: &'a Node, depth: Option<usize>) -> Found<'a> {
    let mut depth = depth;
    let current = depth;

    if child.children.is_empty() {
        return Found {
            sub: None,
            depth: None,
            is_project: false,
        };
    }

    let mut sub = None;
    let mut is_project = false;
    let mut project_fallback = None;

    for v in &child.children {
        if !v.file_paths.is_empty() {
            // a nested project's filePaths point inside its own tree, so prefer a plain file child
            // and fall back to the project, whose *.project.json strips predictably
            if project_json_path(v).is_some() {
                project_fallback = project_fallback.or(Some(v));
            } else {
                sub = Some(v);
            }
        } else {
            let found = try_folder(v, current.map(|d| d + 1));
            sub = found.sub;
            depth = found.depth;
            is_project = found.is_project;
        }

        if sub.is_some() {
            break;
        }
    }

    if sub.is_none() && project_fallback.is_some() {
        sub = project_fallback;
        is_project = true;
        depth = current;
    }

    Found {
        sub,
        depth,
        is_project,
    }
}

/// The directory a sourcemap folder node stands for, derived from its first file descendant.
pub fn folder_path(node: &Node) -> Option<PathBuf> {
    let found = try_folder(node, Some(1));
    let sub = found.sub?;

    let path = if found.is_project {
        project_json_path(sub)?
    } else {
        sub.file_paths.first()?.as_path()
    };

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let is_init = !found.is_project && v1_init_suffix(&name);
    let strips_two = is_init || found.is_project;

    // v1 crashed here on a nil depth (a script-less folder before the file); Rojo's sourcemap
    // never emits one, so treat it as underivable
    let depth = found.depth?;
    let pops = if strips_two { 2 + depth - 1 } else { depth };

    let mut out = path.to_path_buf();

    for _ in 0..pops {
        out.pop();
    }

    Some(out)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeIndex {
    pub root_name: String,
    /// absolute file or folder path to instance path (root name first)
    pub paths: BTreeMap<PathBuf, Vec<String>>,
}

fn register_init(paths: &mut Vec<PathBuf>, dir: &Path, exists: &dyn Fn(&Path) -> bool) {
    let luau = dir.join("init.luau");
    let lua = dir.join("init.lua");

    if exists(&luau) {
        paths.push(luau);
    } else if exists(&lua) {
        paths.push(lua);
    }
}

struct Walk<'a> {
    exists: &'a dyn Fn(&Path) -> bool,
    paths: BTreeMap<PathBuf, Vec<String>>,
    files: usize,
    folders: usize,
}

impl Walk<'_> {
    fn child(&mut self, child: &Node, crumbs: &[String]) {
        let mut found = Vec::new();

        if let Some(first) = child.file_paths.first() {
            self.files += 1;
            found.push(normalize(first));
        } else {
            self.folders += 1;

            match folder_path(child) {
                Some(folder) => {
                    let folder = normalize(&folder);
                    register_init(&mut found, &folder, self.exists);
                    found.insert(0, folder);
                }
                None => log::warn(&format!("Folder '{}' has no files, skipping", child.name)),
            }
        }

        let mut instance = crumbs.to_vec();
        instance.push(child.name.clone());

        for path in found {
            self.paths.insert(path, instance.clone());
        }

        for sub in &child.children {
            self.child(sub, &instance);
        }
    }
}

/// `replaceAliases.inFiles`'s path map (v1.2.4 `replaceAliases.luau:229-340`): every node below
/// the root by its first file path, folders by their derived path plus an existing init file,
/// then the root `$path` and its init as the root instance, never overriding a node.
pub fn index(sm: &Node, root_path: Option<&Path>, exists: &dyn Fn(&Path) -> bool) -> NodeIndex {
    let mut walk = Walk {
        exists,
        paths: BTreeMap::new(),
        files: 0,
        folders: 0,
    };
    let root = vec![sm.name.clone()];

    for child in &sm.children {
        walk.child(child, &root);
    }

    if log::verbose() > 0 {
        log::info(&format!(
            "Sourcemap scan: {} files, {} folders",
            walk.files, walk.folders
        ));
    }

    let mut paths = walk.paths;

    if let Some(root_path) = root_path {
        let root_path = normalize(root_path);
        let luau = root_path.join("init.luau");
        let lua = root_path.join("init.lua");

        paths.entry(root_path).or_insert_with(|| root.clone());

        // v1 falls through to init.lua when init.luau exists but is already registered
        if exists(&luau) && !paths.contains_key(&luau) {
            paths.insert(luau, root.clone());
        } else if exists(&lua) && !paths.contains_key(&lua) {
            paths.insert(lua, root.clone());
        }
    }

    NodeIndex {
        root_name: sm.name.clone(),
        paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `rojo sourcemap --absolute` (7.6.1) on tests/fixtures/nested-init, prefix rewritten to /p
    const NESTED_INIT: &str = r#"{"name":"NestedInit","className":"ModuleScript","filePaths":["/p/src/init.luau","/p/default.project.json"],"children":[{"name":"lib","className":"Folder","children":[{"name":"Foo","className":"ModuleScript","filePaths":["/p/src/lib/Foo/init.luau"],"children":[{"name":"Bar","className":"ModuleScript","filePaths":["/p/src/lib/Foo/Bar.luau"]}]}]},{"name":"pkg","className":"Folder","children":[{"name":"Logger","className":"ModuleScript","filePaths":["/p/src/pkg/Logger.luau"]}]}]}"#;

    // the same on tests/fixtures/nested-project-in-folder
    const NESTED_PROJECT: &str = r#"{"name":"NestedProjectInFolder","className":"ModuleScript","filePaths":["/p/src/init.luau","/p/default.project.json"],"children":[{"name":"mod","className":"Folder","children":[{"name":"user","className":"ModuleScript","filePaths":["/p/src/mod/user.luau"]}]},{"name":"pkg","className":"Folder","children":[{"name":"encoder","className":"ModuleScript","filePaths":["/p/pkg/Encoder/src/init.luau","/p/pkg/Encoder/default.project.json"]},{"name":"Promise","className":"ModuleScript","filePaths":["/p/pkg/Promise.luau"]}]}]}"#;

    fn node(json: &str) -> Node {
        parse(json).unwrap()
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    const ENCODER: &str = r#"{"name":"encoder","className":"ModuleScript","filePaths":["/p/pkg/Encoder/src/init.luau","/p/pkg/Encoder/default.project.json"]}"#;

    #[test]
    fn parse_defaults_missing_vectors() {
        let n = node(r#"{"name":"A","className":"Folder"}"#);
        assert!(n.file_paths.is_empty());
        assert!(n.children.is_empty());
        assert!(parse(r#"{"name":"A"}"#).is_err());
    }

    #[test]
    fn folder_path_cases() {
        let promise =
            r#"{"name":"Promise","className":"ModuleScript","filePaths":["/p/pkg/Promise.luau"]}"#;
        let pkg_both =
            format!(r#"{{"name":"pkg","className":"Folder","children":[{ENCODER},{promise}]}}"#);
        assert_eq!(folder_path(&node(&pkg_both)), Some(PathBuf::from("/p/pkg")));

        let pkg_project =
            format!(r#"{{"name":"pkg","className":"Folder","children":[{ENCODER}]}}"#);
        assert_eq!(
            folder_path(&node(&pkg_project)),
            Some(PathBuf::from("/p/pkg"))
        );

        let lib = r#"{"name":"lib","className":"Folder","children":[{"name":"Foo","className":"ModuleScript","filePaths":["/p/src/lib/Foo/init.luau"]}]}"#;
        assert_eq!(folder_path(&node(lib)), Some(PathBuf::from("/p/src/lib")));

        let nested = r#"{"name":"A","className":"Folder","children":[{"name":"B","className":"Folder","children":[{"name":"c","className":"ModuleScript","filePaths":["/p/A/B/c.luau"]}]}]}"#;
        assert_eq!(folder_path(&node(nested)), Some(PathBuf::from("/p/A")));

        let empty = r#"{"name":"E","className":"Folder"}"#;
        assert_eq!(folder_path(&node(empty)), None);
    }

    #[test]
    fn folder_path_nested_init_and_nested_project() {
        // A holds B holding C/init.luau: init strips two, then one per level below the first
        let init = r#"{"name":"A","className":"Folder","children":[{"name":"B","className":"Folder","children":[{"name":"C","className":"ModuleScript","filePaths":["/p/A/B/C/init.luau"]}]}]}"#;
        assert_eq!(folder_path(&node(init)), Some(PathBuf::from("/p/A")));

        // a project two levels down falls back at its own level
        let deep = format!(
            r#"{{"name":"A","className":"Folder","children":[{{"name":"pkg","className":"Folder","children":[{ENCODER}]}}]}}"#
        );
        assert_eq!(folder_path(&node(&deep)), Some(PathBuf::from("/p")));

        // the whole fixture: pkg prefers the plain file child over the first (project) child
        let sm = node(NESTED_PROJECT);
        assert_eq!(folder_path(&sm.children[1]), Some(PathBuf::from("/p/pkg")));
        assert_eq!(
            folder_path(&sm.children[0]),
            Some(PathBuf::from("/p/src/mod"))
        );
    }

    #[test]
    fn index_registers_roots_and_inits() {
        let sm = node(NESTED_INIT);
        let exists = |p: &Path| p == Path::new("/p/src/init.luau");
        let idx = index(&sm, Some(Path::new("/p/src")), &exists);
        let at = |p: &str| idx.paths.get(Path::new(p)).cloned();

        assert_eq!(idx.root_name, "NestedInit");
        assert_eq!(at("/p/src"), Some(v(&["NestedInit"])));
        assert_eq!(at("/p/src/init.luau"), Some(v(&["NestedInit"])));
        assert_eq!(at("/p/src/lib"), Some(v(&["NestedInit", "lib"])));
        assert_eq!(
            at("/p/src/lib/Foo/init.luau"),
            Some(v(&["NestedInit", "lib", "Foo"]))
        );
        assert_eq!(
            at("/p/src/lib/Foo/Bar.luau"),
            Some(v(&["NestedInit", "lib", "Foo", "Bar"]))
        );
        assert_eq!(at("/p/src/pkg"), Some(v(&["NestedInit", "pkg"])));
        assert_eq!(
            at("/p/src/pkg/Logger.luau"),
            Some(v(&["NestedInit", "pkg", "Logger"]))
        );
        assert_eq!(idx.paths.len(), 7);
    }

    #[test]
    fn index_folder_inits_and_root_without_path() {
        let sm = node(NESTED_PROJECT);
        let exists = |p: &Path| p == Path::new("/p/pkg/init.lua");
        let idx = index(&sm, None, &exists);
        let at = |p: &str| idx.paths.get(Path::new(p)).cloned();

        assert_eq!(at("/p/pkg"), Some(v(&["NestedProjectInFolder", "pkg"])));
        assert_eq!(
            at("/p/pkg/init.lua"),
            Some(v(&["NestedProjectInFolder", "pkg"]))
        );
        assert_eq!(
            at("/p/pkg/Encoder/src/init.luau"),
            Some(v(&["NestedProjectInFolder", "pkg", "encoder"]))
        );
        assert_eq!(at("/p/src"), None);
        assert_eq!(at("/p/src/init.luau"), None);
    }

    #[test]
    fn index_root_never_overrides_a_registered_path() {
        let sm = node(NESTED_INIT);
        let idx = index(&sm, Some(Path::new("/p/src/lib")), &|_| true);
        assert_eq!(
            idx.paths[Path::new("/p/src/lib")],
            v(&["NestedInit", "lib"])
        );
        // init.luau of the root is taken by the folder's own init registration
        assert_eq!(
            idx.paths[Path::new("/p/src/lib/init.luau")],
            v(&["NestedInit", "lib"])
        );
        assert_eq!(
            idx.paths[Path::new("/p/src/lib/init.lua")],
            v(&["NestedInit"])
        );
    }
}
