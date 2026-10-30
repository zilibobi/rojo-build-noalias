use anyhow::Context;
use std::path::{Path, PathBuf};

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
