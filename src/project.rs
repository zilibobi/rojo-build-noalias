use crate::{log, paths};
use anyhow::Context;
use serde_json::Value;
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
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let stem = &name[..name.len() - kind_suffix(&name).len()];
    let meta = file.with_file_name(format!("{stem}.meta.json"));

    if meta.is_file() {
        move_entry(&meta, &root.join(format!("{key}.meta.json")))?;
    }

    Ok(())
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
}
