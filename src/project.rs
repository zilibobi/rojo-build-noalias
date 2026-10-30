use crate::{log, paths};
use anyhow::Context;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

/// The project file names tried, in order, when the CLI named none for this directory.
const CANDIDATES: [&str; 4] = [
    "default.project.json",
    "default.project.jsonc",
    "project.json",
    "project.jsonc",
];

/// `util.findProjectFileForLuaurc` (v1.2.4 `util.luau:448-484`): the project file of a `.luaurc`
/// in `dir`. `preferred` (the CLI's file name) first, then the four fixed names, then the first
/// `*.project.json(c)` by name.
pub fn find_for_luaurc(dir: &Path, preferred: Option<&str>) -> Option<PathBuf> {
    let fixed = preferred.into_iter().chain(CANDIDATES);

    if let Some(found) = fixed.map(|name| dir.join(name)).find(|p| p.is_file()) {
        return Some(found);
    }

    // v1.2.4 took the first readDir entry; 2.0 takes the first by name (divergence 8)
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".project.json") || n.ends_with(".project.jsonc"))
        .collect();

    names.sort();
    names.first().map(|n| dir.join(n))
}

/// A project file parsed as JSONC, the way Rojo reads it.
pub fn read(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let options = jsonc_parser::ParseOptions::default();
    let value: serde_json::Value = jsonc_parser::parse_to_serde_value(&text, &options)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("cannot parse {}", path.display()))?;

    if !value.is_object() {
        return Err(anyhow::anyhow!("expected a JSON object at the top level"))
            .with_context(|| format!("cannot parse {}", path.display()));
    }

    Ok(value)
}

/// The root `$path` of a project: a string, or `{"optional": <string>}`.
pub fn root_path(project: &serde_json::Value) -> Option<String> {
    let path = project.get("tree")?.get("$path")?;

    path.as_str()
        .or_else(|| path.get("optional")?.as_str())
        .map(str::to_string)
}

/// The middle names Rojo strips with the extension when it names an instance from a file
/// (`run.server.luau` is `run`), measured on Rojo 7.6.1.
const KIND_SUFFIXES: [&str; 4] = ["server", "client", "plugin", "model"];

/// `util.flattenTreePaths` (v1.2.4 `util.luau:498-594`): every direct child of the tree whose
/// `$path` lies outside the root `$path` moves into `<root>/<key>`, so that the converted files
/// sit where the instance tree puts them. An entry that only named the path is dropped (the
/// root `$path` mounts it now); one with more is kept with the new path and a warning. Paths
/// resolve against the project file's own directory, and nothing outside `work` is touched.
/// Returns true when the project file was rewritten.
pub fn flatten_tree_paths(work: &Path, project_path: &Path) -> anyhow::Result<bool> {
    Ok(!flatten(work, project_path)?.is_empty())
}

/// One directory or file [`flatten`] moved, both paths normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// [`flatten_tree_paths`], returning what moved; the project file was rewritten when any did.
pub fn flatten(work: &Path, project_path: &Path) -> anyhow::Result<Vec<Move>> {
    // a project that does not parse was already reported when its scope was built
    let Ok(mut project) = read(project_path) else {
        return Ok(Vec::new());
    };

    let Some(root) = root_path(&project) else {
        return Ok(Vec::new());
    };

    let root = root.trim_end_matches('/').to_string();

    if root.is_empty() {
        return Ok(Vec::new());
    }

    let base = project_path.parent().unwrap_or(Path::new(""));
    let work = paths::normalize(work);
    let root_rel = paths::normalize(Path::new(&root));
    let root_abs = paths::normalize(&base.join(&root_rel));

    if !root_abs.starts_with(&work) || !root_abs.is_dir() {
        return Ok(Vec::new());
    }

    let Some(tree) = project.get_mut("tree").and_then(Value::as_object_mut) else {
        return Ok(Vec::new());
    };

    let keys: Vec<String> = tree
        .keys()
        .filter(|k| !k.starts_with('$'))
        .cloned()
        .collect();
    let mut moved = Vec::new();

    for key in keys {
        let Some(child) = tree.get(&key).and_then(Value::as_object) else {
            continue;
        };

        let Some(raw) = child.get("$path").and_then(path_text) else {
            continue;
        };

        let child_rel = paths::normalize(Path::new(raw.trim_end_matches('/')));

        if child_rel.starts_with(&root_rel) {
            continue;
        }

        let src = paths::normalize(&base.join(&child_rel));
        let single_name = matches!(
            Path::new(&key).components().collect::<Vec<_>>()[..],
            [Component::Normal(_)]
        );
        let movable = single_name
            && src.starts_with(&work)
            && src != work
            && !root_abs.starts_with(&src)
            && !is_project_file(&src);

        if !movable {
            log::warn(&format!(
                "\"{key}\" cannot be moved into \"{root}\", so its project entry is left as it is"
            ));
            continue;
        }

        // a missing optional path is fine as written, and a missing required one fails in Rojo
        // as it did before the conversion
        if !src.exists() {
            continue;
        }

        let is_dir = src.is_dir();
        let name = if is_dir {
            key.clone()
        } else {
            let file = src.file_name().unwrap_or_default().to_string_lossy();
            format!("{key}{}", kind_suffix(&file))
        };

        let to = root_abs.join(&name);
        move_entry(&src, &to)?;

        if !is_dir {
            move_meta(&src, &root_abs, &key)?;
        }

        let dest = format!("{root}/{name}");
        let only_path = child.len() == 1;

        if only_path {
            tree.shift_remove(&key);
        } else {
            log::warn(&format!(
                "\"{key}\" was moved into \"{root}\" but its project entry carries more than a $path, so it is kept and Rojo will build two instances named \"{key}\""
            ));

            let path = &mut tree[&key]["$path"];

            match path.get_mut("optional") {
                Some(optional) => *optional = Value::String(dest),
                None => *path = Value::String(dest),
            }
        }

        moved.push(Move { from: src, to });
    }

    if !moved.is_empty() {
        // pretty JSON without a trailing newline, as v1.2.4's serde.encode wrote it
        let text = serde_json::to_string_pretty(&project)?;
        std::fs::write(project_path, text)
            .with_context(|| format!("cannot write {}", project_path.display()))?;
    }

    Ok(moved)
}

/// Every `$path` in the project's tree at any depth, the root's included, resolved against the
/// project file's directory and normalized. Empty when the project does not parse.
pub fn mounts(project_path: &Path) -> Vec<PathBuf> {
    fn walk(node: &Value, base: &Path, out: &mut Vec<PathBuf>) {
        let Some(node) = node.as_object() else {
            return;
        };

        if let Some(path) = node.get("$path").and_then(path_text) {
            out.push(paths::normalize(&base.join(path)));
        }

        for (key, child) in node {
            if !key.starts_with('$') {
                walk(child, base, out);
            }
        }
    }

    let mut out = Vec::new();

    if let (Ok(project), Some(base)) = (read(project_path), project_path.parent()) {
        walk(&project["tree"], base, &mut out);
    }

    out
}

/// A `$path` value: a string, or `{"optional": <string>}`.
fn path_text(path: &Value) -> Option<&str> {
    path.as_str().or_else(|| path.get("optional")?.as_str())
}

fn is_project_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".project.json") || n.ends_with(".project.jsonc"))
}

/// The end of a file name Rojo drops to name the instance: `.server.luau`, `.model.json` and
/// the like, else the last extension.
fn kind_suffix(name: &str) -> &str {
    let mut dots = name.rmatch_indices('.').map(|(i, _)| i);

    let Some(last) = dots.next() else {
        return "";
    };

    match dots.next() {
        Some(prev) if KIND_SUFFIXES.contains(&&name[prev + 1..last]) => &name[prev..],
        _ => &name[last..],
    }
}

/// Moves `src` to `dest`, replacing whatever was there.
fn move_entry(src: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.is_dir() {
        std::fs::remove_dir_all(dest)
            .with_context(|| format!("cannot remove {}", dest.display()))?;
    } else if dest.exists() {
        std::fs::remove_file(dest).with_context(|| format!("cannot remove {}", dest.display()))?;
    }

    std::fs::rename(src, dest)
        .with_context(|| format!("cannot move {} to {}", src.display(), dest.display()))
}

/// Moves the `<stem>.meta.json` Rojo reads beside a mounted file along with it, renamed for the
/// file's new instance name.
fn move_meta(file: &Path, root: &Path, key: &str) -> anyhow::Result<()> {
    let meta = meta_of(file);

    if meta.is_file() {
        move_entry(&meta, &root.join(format!("{key}.meta.json")))?;
    }

    Ok(())
}

/// What a build reads, relative to the project directory (spec 6.5): the mounted directories and
/// files (`roots`), every project file Rojo or a scope reads (`projects`) and the `.luaurc` files
/// whose scopes can rewrite a mounted script (`luaurcs`). `files` are the roots that were regular
/// files when the closure was computed; a root that is neither did not exist then.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mounts {
    pub roots: BTreeSet<PathBuf>,
    pub projects: BTreeSet<PathBuf>,
    pub luaurcs: BTreeSet<PathBuf>,
    pub files: BTreeSet<PathBuf>,
}

impl Mounts {
    /// Whether the build reads `rel`: a path under a root and under no skip name below it, the
    /// meta file of a file root, a project file or a `.luaurc`.
    pub fn contains(&self, rel: &Path) -> bool {
        if self.projects.contains(rel) || self.luaurcs.contains(rel) {
            return true;
        }

        let under_root = self.roots.iter().any(|root| {
            rel.strip_prefix(root)
                .is_ok_and(|rest| !paths::skipped(rest))
        });

        under_root || self.files.iter().any(|f| meta_of(f) == rel)
    }

    /// The directories to watch: recursively every directory root not inside another one, and
    /// shallowly every directory above a root, a project file or a `.luaurc` that no recursive one
    /// covers (`""` is the project directory). A root that did not exist is listed as recursive.
    pub fn watch_dirs(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let dirs: Vec<&PathBuf> = self
            .roots
            .iter()
            .filter(|r| !self.files.contains(*r))
            .collect();
        let deep: Vec<PathBuf> = dirs
            .iter()
            .filter(|r| !dirs.iter().any(|o| o != *r && r.starts_with(o)))
            .map(|r| (*r).clone())
            .collect();

        let mut shallow = BTreeSet::new();

        for path in self.roots.iter().chain(&self.projects).chain(&self.luaurcs) {
            let mut dir = path.parent();

            while let Some(d) = dir {
                shallow.insert(d.to_path_buf());
                dir = d.parent();
            }
        }

        shallow.retain(|d| !deep.iter().any(|r| d.starts_with(r)));

        (deep, shallow.into_iter().collect())
    }
}

/// The `<stem>.meta.json` Rojo reads beside a file it mounts (`run.server.luau` reads
/// `run.meta.json`, measured on Rojo 7.6.1).
pub fn meta_of(file: &Path) -> PathBuf {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let stem = &name[..name.len() - kind_suffix(&name).len()];
    file.with_file_name(format!("{stem}.meta.json"))
}

/// The mount closure of the project file `cli_project` (relative to `project_dir` or absolute),
/// spec 6.5. `preferred` is the CLI's project file name, which counts only for a `.luaurc` in the
/// project directory.
///
/// The projects Rojo reads (the CLI's, a `$path` naming a `*.project.json`, and a
/// `default.project.json` in a mounted directory, where Rojo stops walking) must parse and must
/// not name a `$path` outside the project directory. A scope project only the scopes read (the
/// project of a `.luaurc`, and what it reaches) is taken leniently: when it does not parse it is
/// still listed, so its scope reports it as v1.2.4 did, and an outside path is left out.
pub fn mount_closure(
    project_dir: &Path,
    cli_project: &Path,
    preferred: Option<&str>,
) -> anyhow::Result<Mounts> {
    let mut c = Closure {
        dir: paths::normalize(project_dir),
        mounts: Mounts::default(),
        queue: VecDeque::new(),
    };
    let mut looked_up = BTreeSet::new();

    c.queue
        .push_back((paths::normalize(&c.dir.join(cli_project)), true));

    loop {
        while let Some((project, strict)) = c.queue.pop_front() {
            c.project(&project, strict)?;
        }

        let fresh: Vec<PathBuf> = c
            .mounts
            .luaurcs
            .iter()
            .filter(|l| !looked_up.contains(*l))
            .cloned()
            .collect();

        if fresh.is_empty() {
            return Ok(c.mounts);
        }

        for rel in fresh {
            let dir_rel = rel.parent().unwrap_or(Path::new("")).to_path_buf();
            let preferred = preferred.filter(|_| dir_rel.as_os_str().is_empty());

            if let Some(found) = find_for_luaurc(&c.dir.join(&dir_rel), preferred) {
                c.queue.push_back((paths::normalize(&found), false));
            }

            looked_up.insert(rel);
        }
    }
}

struct Closure {
    dir: PathBuf,
    mounts: Mounts,
    /// absolute project files still to read, and whether Rojo reads them
    queue: VecDeque<(PathBuf, bool)>,
}

impl Closure {
    fn project(&mut self, abs: &Path, strict: bool) -> anyhow::Result<()> {
        let Ok(rel) = abs.strip_prefix(&self.dir) else {
            if strict {
                anyhow::bail!(
                    "the project file {} is outside the project directory {}",
                    abs.display(),
                    self.dir.display()
                );
            }

            return Ok(());
        };

        if !self.mounts.projects.insert(rel.to_path_buf()) {
            return Ok(());
        }

        let value = match read(abs) {
            Ok(value) => value,
            Err(e) if strict => return Err(e),
            Err(_) => return Ok(()),
        };

        let mut texts = Vec::new();
        tree_paths(&value["tree"], &mut texts);

        let base = abs.parent().unwrap_or(&self.dir).to_path_buf();
        let shown = paths::slash(rel);

        for text in texts {
            let target = paths::normalize(&base.join(&text));

            let Ok(target_rel) = target.strip_prefix(&self.dir) else {
                if strict {
                    anyhow::bail!(
                        "the $path \"{text}\" in {shown} resolves outside the project directory {}",
                        self.dir.display()
                    );
                }

                continue;
            };

            if is_project_file(&target) {
                if target.is_file() {
                    self.queue.push_back((target.clone(), strict));
                }

                continue;
            }

            self.root(target_rel.to_path_buf(), &target, strict);
        }

        Ok(())
    }

    fn root(&mut self, rel: PathBuf, abs: &Path, strict: bool) {
        if !self.mounts.roots.insert(rel.clone()) {
            return;
        }

        let mut dir = rel.parent();

        while let Some(d) = dir {
            let luaurc = d.join(".luaurc");

            if self.dir.join(&luaurc).is_file() {
                self.mounts.luaurcs.insert(luaurc);
            }

            dir = d.parent();
        }

        if abs.is_file() {
            self.mounts.files.insert(rel);
            return;
        }

        if !abs.is_dir() {
            return;
        }

        // Rojo reads a directory holding a default.project.json as that project and walks no
        // further into it; the `.luaurc` files are taken from the whole directory
        let mut nested: Vec<PathBuf> = Vec::new();

        let walk = walkdir::WalkDir::new(abs)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0 || !paths::SKIP_NAMES.iter().any(|n| e.file_name() == *n)
            });

        for entry in walk.flatten() {
            let path = entry.path();

            if entry.file_type().is_dir() {
                let project = path.join("default.project.json");

                if !nested.iter().any(|n| path.starts_with(n)) && project.is_file() {
                    nested.push(path.to_path_buf());
                    self.queue.push_back((paths::normalize(&project), strict));
                }

                continue;
            }

            if entry.file_name() == ".luaurc"
                && let Ok(inner) = path.strip_prefix(abs)
            {
                self.mounts.luaurcs.insert(rel.join(inner));
            }
        }
    }
}

/// Every `$path` in a project tree, at any depth, as written.
fn tree_paths(node: &Value, out: &mut Vec<String>) {
    let Some(node) = node.as_object() else {
        return;
    };

    if let Some(path) = node.get("$path").and_then(path_text) {
        out.push(path.to_string());
    }

    for (key, child) in node {
        if !key.starts_with('$') {
            tree_paths(child, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_for_luaurc_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let touch = |name: &str| std::fs::write(dir.join(name), "{}").unwrap();
        let found = |preferred| find_for_luaurc(dir, preferred);

        assert_eq!(found(None), None);

        touch("b.project.jsonc");
        touch("a.project.json");
        touch("notes.json");
        assert_eq!(found(None), Some(dir.join("a.project.json")));

        for name in CANDIDATES.iter().rev() {
            touch(name);
            assert_eq!(found(None), Some(dir.join(name)));
        }

        touch("dev.project.json");
        assert_eq!(
            found(Some("dev.project.json")),
            Some(dir.join("dev.project.json"))
        );
        // a preferred name that is not there falls back to the fixed names
        assert_eq!(
            found(Some("gone.project.json")),
            Some(dir.join("default.project.json"))
        );
    }

    #[test]
    fn read_takes_jsonc_and_keeps_key_order() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("default.project.json");
        std::fs::write(
            &path,
            "{\n  // a comment\n  \"name\": \"X\",\n  \"tree\": {\"$path\": \"src\",},\n  \"emitLegacyScripts\": false,\n}\n",
        )
        .unwrap();

        let value = read(&path).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["name", "tree", "emitLegacyScripts"]);
        assert_eq!(value["tree"]["$path"], "src");

        std::fs::write(&path, "[]").unwrap();
        assert!(read(&path).is_err());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read(&path).is_err());
        assert!(read(&tmp.path().join("missing.json")).is_err());
    }

    #[test]
    fn kind_suffix_matches_rojo_naming() {
        assert_eq!(kind_suffix("run.server.luau"), ".server.luau");
        assert_eq!(kind_suffix("ui.client.lua"), ".client.lua");
        assert_eq!(kind_suffix("tool.plugin.luau"), ".plugin.luau");
        assert_eq!(kind_suffix("part.model.json"), ".model.json");
        assert_eq!(kind_suffix("Promise.luau"), ".luau");
        // Rojo 7.6.1 keeps an unknown middle name in the instance name
        assert_eq!(kind_suffix("a.local.luau"), ".luau");
        assert_eq!(kind_suffix("notes.txt"), ".txt");
        assert_eq!(kind_suffix("LICENSE"), "");
    }

    #[test]
    fn root_path_forms() {
        assert_eq!(
            root_path(&json!({"tree": {"$path": "src"}})).as_deref(),
            Some("src")
        );
        assert_eq!(
            root_path(&json!({"tree": {"$path": {"optional": "lib"}}})).as_deref(),
            Some("lib")
        );
        assert_eq!(
            root_path(&json!({"tree": {"$className": "DataModel"}})),
            None
        );
        assert_eq!(root_path(&json!({"tree": {"$path": 3}})), None);
        assert_eq!(root_path(&json!({})), None);
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn set(items: &[&str]) -> BTreeSet<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn mount_closure_of_normal_build() {
        let dir = fixture("normal-build");
        let m = mount_closure(&dir, &dir.join("default.project.json"), None).unwrap();
        assert_eq!(m.roots, set(&["src", "pkg"]));
        assert_eq!(m.projects, set(&["default.project.json"]));
        assert_eq!(m.luaurcs, set(&[".luaurc"]));
        assert!(m.files.is_empty());
    }

    #[test]
    fn mount_closure_takes_nested_projects() {
        let dir = fixture("nested-project-in-folder");
        let m = mount_closure(&dir, Path::new("default.project.json"), None).unwrap();
        assert_eq!(
            m.projects,
            set(&["default.project.json", "pkg/Encoder/default.project.json"])
        );
        assert_eq!(m.roots, set(&["src", "pkg", "pkg/Encoder/src"]));
        assert_eq!(m.luaurcs, set(&[".luaurc"]));
    }

    #[test]
    fn mount_closure_takes_the_named_project_only() {
        let dir = fixture("named-project");
        let m = mount_closure(
            &dir,
            &dir.join("dev.project.json"),
            Some("dev.project.json"),
        )
        .unwrap();
        assert_eq!(m.projects, set(&["dev.project.json"]));
        assert_eq!(m.roots, set(&["src"]));
    }

    #[test]
    fn mount_closure_takes_ancestor_and_nested_luaurcs() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        put(
            &t.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src", "shared": {"$path": "vendor/shared"}, "run": {"$path": "lib/a/run.server.luau"}}}"#,
        );
        put(&t.join("src/init.luau"), "");
        put(&t.join("src/worker/.luaurc"), "{}");
        put(&t.join("src/node_modules/.luaurc"), "{}");
        put(&t.join("vendor/.luaurc"), "{}");
        put(&t.join("vendor/shared/x.luau"), "");
        put(&t.join("lib/.luaurc"), "{}");
        // a scope project that does not parse is mirrored for its scope to report
        put(&t.join("lib/default.project.json"), "{ not json");
        put(&t.join("lib/a/run.server.luau"), "");
        put(&t.join("other/.luaurc"), "{}");

        let m = mount_closure(t, &t.join("default.project.json"), None).unwrap();
        assert_eq!(
            m.luaurcs,
            set(&["src/worker/.luaurc", "vendor/.luaurc", "lib/.luaurc"])
        );
        assert_eq!(m.files, set(&["lib/a/run.server.luau"]));
        assert_eq!(
            m.projects,
            set(&["default.project.json", "lib/default.project.json"])
        );
    }

    #[test]
    fn mount_closure_follows_scope_projects() {
        // the scope project of vendor/.luaurc mounts a dir the CLI project does not
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        put(
            &t.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src", "shared": {"$path": "vendor/shared"}}}"#,
        );
        put(&t.join("src/init.luau"), "");
        put(
            &t.join("vendor/.luaurc"),
            r#"{"aliases": {"t": "./types"}}"#,
        );
        put(
            &t.join("vendor/default.project.json"),
            r#"{"name": "V", "tree": {"$path": "shared", "types": {"$path": "types"}, "far": {"$path": "../../far"}}}"#,
        );
        put(&t.join("vendor/types/.luaurc"), "{}");
        put(&t.join("vendor/shared/x.luau"), "");

        let m = mount_closure(t, &t.join("default.project.json"), None).unwrap();
        assert_eq!(
            m.projects,
            set(&["default.project.json", "vendor/default.project.json"])
        );
        // an outside path in a project Rojo itself never reads is left out, not an error
        assert_eq!(m.roots, set(&["src", "vendor/shared", "vendor/types"]));
        assert_eq!(m.luaurcs, set(&["vendor/.luaurc", "vendor/types/.luaurc"]));
    }

    #[test]
    fn mount_closure_reads_path_projects_and_optional_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        put(
            &t.join("default.project.json"),
            r#"{"name": "X", "tree": {"$className": "Folder", "lib": {"$path": "sub/lib.project.json"}, "opt": {"$path": {"optional": "maybe"}}}}"#,
        );
        put(
            &t.join("sub/lib.project.json"),
            r#"{"name": "L", "tree": {"$path": "code"}}"#,
        );
        put(&t.join("sub/code/init.luau"), "");

        let m = mount_closure(t, &t.join("default.project.json"), None).unwrap();
        assert_eq!(
            m.projects,
            set(&["default.project.json", "sub/lib.project.json"])
        );
        assert_eq!(m.roots, set(&["sub/code", "maybe"]));
    }

    #[test]
    fn nested_projects_stop_at_the_first_project_dir() {
        // Rojo reads pkg/A as a project and never walks pkg/A/other
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        put(
            &t.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src", "pkg": {"$path": "pkg"}}}"#,
        );
        put(&t.join("src/init.luau"), "");
        put(
            &t.join("pkg/A/default.project.json"),
            r#"{"name": "A", "tree": {"$path": "src"}}"#,
        );
        put(&t.join("pkg/A/src/init.luau"), "");
        put(
            &t.join("pkg/A/other/default.project.json"),
            r#"{"name": "O", "tree": {"$path": "../../../../outside"}}"#,
        );

        let m = mount_closure(t, &t.join("default.project.json"), None).unwrap();
        assert_eq!(
            m.projects,
            set(&["default.project.json", "pkg/A/default.project.json"])
        );
    }

    #[test]
    fn outside_paths_and_broken_projects_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("p");
        put(
            &t.join("default.project.json"),
            r#"{"name": "X", "tree": {"$path": "src", "o": {"$path": {"optional": "../outside"}}}}"#,
        );
        let err = format!(
            "{:#}",
            mount_closure(&t, &t.join("default.project.json"), None).unwrap_err()
        );
        assert!(err.contains("outside the project directory"), "{err}");
        assert!(err.contains("\"../outside\""), "{err}");

        put(&t.join("default.project.json"), "{ not json");
        let err = format!(
            "{:#}",
            mount_closure(&t, &t.join("default.project.json"), None).unwrap_err()
        );
        assert!(err.contains("cannot parse"), "{err}");

        let err = mount_closure(&t, &t.join("gone.project.json"), None).unwrap_err();
        assert!(format!("{err:#}").contains("cannot read"), "{err:#}");
    }

    #[test]
    fn mounts_contains_and_watch_dirs() {
        let m = Mounts {
            roots: set(&[
                "src",
                "vendor/shared",
                "lib/run.server.luau",
                "node_modules/pkgs",
            ]),
            projects: set(&["default.project.json", "pkg/A/default.project.json"]),
            luaurcs: set(&[".luaurc", "vendor/.luaurc"]),
            files: set(&["lib/run.server.luau"]),
        };

        for yes in [
            "src",
            "src/a/b.luau",
            "vendor/shared/x",
            "lib/run.server.luau",
            "lib/run.meta.json",
            "node_modules/pkgs/Thing.luau",
            "default.project.json",
            "pkg/A/default.project.json",
            "vendor/.luaurc",
        ] {
            assert!(m.contains(Path::new(yes)), "{yes}");
        }

        for no in [
            "",
            "vendor",
            "vendor/other",
            "lib/other.luau",
            "src/node_modules/x.luau",
            "src/a/.git/HEAD",
            "srcx/a.luau",
            "pkg/A/src/x.luau",
        ] {
            assert!(!m.contains(Path::new(no)), "{no}");
        }

        let (deep, shallow) = m.watch_dirs();
        let deep: BTreeSet<PathBuf> = deep.into_iter().collect();
        let shallow: BTreeSet<PathBuf> = shallow.into_iter().collect();
        assert_eq!(deep, set(&["src", "vendor/shared", "node_modules/pkgs"]));
        assert_eq!(
            shallow,
            set(&["", "vendor", "lib", "node_modules", "pkg", "pkg/A"])
        );
    }
}
